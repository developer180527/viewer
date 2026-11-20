// The stress runner: `-stress all` (or `-stress pdf-scroll,mesh-orbit`) at launch, and
// the app drives itself through each kind of load it can meet, with synthetic input
// into the real canvas, and prints one JSON line per scenario (`viewer-stress: {…}`).
// The same code runs on the Mac and on iPhone and iPad; scripts/stress/ copies the
// files (target/stress/files) into Documents, runs it, profiles it, and tabulates it.
//
// Each line holds, for the scenario's measured span: frames drawn and the time each
// draw took on the main thread (p50, p95, p99, max, over the 16.7 ms budget); the
// process's CPU time (every thread: decoders, tile renderers, shader compiles); its
// energy and GPU time, from the kernel's own accounting (task_power_info_v2); its
// wakeups; and its memory footprint (start, peak, end).

import Foundation
import QuartzCore
#if os(macOS)
import AppKit
#else
import UIKit
#endif

/// What the runner drives: the view hosting the canvas (Mac/Canvas.swift,
/// Mobile/Canvas.swift).
@MainActor
protocol StressTarget: AnyObject {
    var stressCanvas: ViewerCanvas? { get }
    /// The canvas's size in points.
    var stressSize: CGSize { get }
    func redraw()
}

/// The kernel's counters for this process, at one moment.
struct Probe {
    var time = CACurrentMediaTime()
    var cpuNs: UInt64 = 0
    var energyNj: UInt64 = 0
    var gpuNs: UInt64 = 0
    var wakeups: UInt64 = 0
    var idleWakeups: UInt64 = 0
    var footprint: UInt64 = 0

    static func now() -> Probe {
        var p = Probe()
        var info = task_power_info_v2_data_t()
        var count = mach_msg_type_number_t(MemoryLayout<task_power_info_v2_data_t>.size / MemoryLayout<natural_t>.size)
        let kr = withUnsafeMutablePointer(to: &info) {
            $0.withMemoryRebound(to: integer_t.self, capacity: Int(count)) { task_info(mach_task_self_, task_flavor_t(TASK_POWER_INFO_V2), $0, &count) }
        }
        if kr == KERN_SUCCESS {
            // CPU and GPU times are in Mach time units (41.67 ns on Apple silicon).
            p.cpuNs = machToNs(info.cpu_energy.total_user + info.cpu_energy.total_system)
            p.energyNj = info.task_energy
            p.gpuNs = machToNs(info.gpu_energy.task_gpu_utilisation)
            p.wakeups = info.cpu_energy.task_interrupt_wakeups
            p.idleWakeups = info.cpu_energy.task_platform_idle_wakeups
        }
        p.footprint = Metrics.footprintBytes()
        return p
    }
}

private let timebase: (UInt64, UInt64) = {
    var tb = mach_timebase_info_data_t()
    mach_timebase_info(&tb)
    return (UInt64(tb.numer), UInt64(max(tb.denom, 1)))
}()

private func machToNs(_ t: UInt64) -> UInt64 { t / timebase.1 * timebase.0 + t % timebase.1 * timebase.0 / timebase.1 }

/// One scenario's measurements.
@MainActor
final class Measure {
    private let start = Probe.now()
    private var frames: [Double] = []
    private var peak: UInt64 = 0
    private var sampler: Timer?
    var extra: [String: Any] = [:]

    init() {
        peak = start.footprint
        Stress.frameSink = { [weak self] ms in self?.frames.append(ms) }
        // Memory peaks between draws too (decoders): sample it.
        sampler = Timer.scheduledTimer(withTimeInterval: 0.1, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { self?.peak = max(self?.peak ?? 0, Metrics.footprintBytes()) }
        }
    }

    func finish(_ name: String, file: String?) -> [String: Any] {
        sampler?.invalidate()
        Stress.frameSink = nil
        let end = Probe.now()
        let secs = max(end.time - start.time, 1e-3)
        let sorted = frames.sorted()
        func pct(_ q: Double) -> Double { sorted.isEmpty ? 0 : sorted[min(sorted.count - 1, Int(Double(sorted.count) * q))] }
        let mb = { (b: UInt64) in Double(b) / 1_048_576 }
        var line: [String: Any] = [
            "scenario": name,
            "file": file ?? "",
            "seconds": round1(secs),
            "frames": frames.count,
            "fps": round1(Double(frames.count) / secs),
            "draw_p50_ms": round2(pct(0.5)),
            "draw_p95_ms": round2(pct(0.95)),
            "draw_p99_ms": round2(pct(0.99)),
            "draw_max_ms": round2(sorted.last ?? 0),
            "draws_over_16ms": frames.filter { $0 > 16.7 }.count,
            "cpu_ms": round1(Double(end.cpuNs - start.cpuNs) / 1e6),
            "cpu_pct": round1(Double(end.cpuNs - start.cpuNs) / 1e9 / secs * 100),
            "energy_mj": round1(Double(end.energyNj &- start.energyNj) / 1e6),
            "power_mw": round1(Double(end.energyNj &- start.energyNj) / 1e6 / secs),
            "gpu_ms": round1(Double(end.gpuNs &- start.gpuNs) / 1e6),
            "wakeups_per_s": round1(Double(end.wakeups - start.wakeups) / secs),
            "idle_wakeups_per_s": round1(Double(end.idleWakeups - start.idleWakeups) / secs),
            "mem_start_mb": round1(mb(start.footprint)),
            "mem_peak_mb": round1(mb(max(peak, end.footprint))),
            "mem_end_mb": round1(mb(end.footprint)),
        ]
        line.merge(extra) { _, new in new }
        return line
    }
}

private func round1(_ v: Double) -> Double { (v * 10).rounded() / 10 }
private func round2(_ v: Double) -> Double { (v * 100).rounded() / 100 }

enum Stress {
    /// Each canvas draw's duration in ms, while a scenario measures.
    nonisolated(unsafe) static var frameSink: ((Double) -> Void)?

    /// Called by the canvas views around `draw()`.
    static func frame(_ ms: Double) { frameSink?(ms) }

    static func emit(_ line: [String: Any]) {
        guard let data = try? JSONSerialization.data(withJSONObject: line, options: [.sortedKeys]),
              let text = String(data: data, encoding: .utf8) else { return }
        print("viewer-stress: \(text)")
        fflush(stdout)
    }
}

/// Runs the scenarios named at launch, one after another, then quits.
@MainActor
final class StressRunner {
    private unowned let shell: Shell
    private let names: [String]
    private let docs = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]

    init(shell: Shell, names: [String]) {
        self.shell = shell
        self.names = names
    }

    private var target: StressTarget? { shell.stressTarget }
    private var canvas: ViewerCanvas? { target?.stressCanvas }
    private var center: (Float, Float) {
        let s = target?.stressSize ?? CGSize(width: 400, height: 400)
        return (Float(s.width / 2), Float(s.height / 2))
    }

    // MARK: the scenarios

    /// Every scenario, in the order `all` runs them.
    private var scenarios: [(String, () async -> Void)] {
        [
            ("idle-pdf", { await self.idle("geotopo.pdf", seconds: 8) }),
            ("pdf-scroll", { await self.scroll("geotopo.pdf", seconds: 10, speed: 40) }),
            ("pdf-fling-touch", { await self.flingTouch("geotopo.pdf", seconds: 8) }),
            ("pdf-zoom", { await self.zoom("geotopo.pdf", seconds: 8) }),
            ("pdf-zoom-total", { await self.zoom("geotopo.pdf", seconds: 8, rest: 4) }),
            ("pdf-page-keys", { await self.keys("geotopo.pdf", seconds: 8, usage: 0x4E) }),
            ("image-zoom-pan", { await self.zoomPan("huge.png", seconds: 8) }),
            ("photo-zoom-pan", { await self.zoomPan("photo.jpg", seconds: 6) }),
            ("mesh-orbit", { await self.orbit("dragon.obj", seconds: 10) }),
            ("textured-orbit", { await self.orbit("mosquito.glb", seconds: 10) }),
            ("scene-orbit", { await self.orbit("sponza/Sponza.gltf", seconds: 10) }),
            ("usdz-orbit", { await self.orbit("helmet.usdz", seconds: 6) }),
            ("docx-scroll", { await self.scroll("document.docx", seconds: 8, speed: 30) }),
            ("pptx-scroll", { await self.scroll("slides.pptx", seconds: 8, speed: 30) }),
            ("markdown-scroll", { await self.scroll("changelog.md", seconds: 8, speed: 30) }),
            ("tree-scroll", { await self.scroll("big.json", seconds: 8, speed: 30) }),
            ("hex-scroll", { await self.scroll("big.log", view: "Raw", seconds: 8, speed: 60) }),
            ("text-fetch", { await self.fetch("big.log", seconds: 5) }),
            ("table-fetch", { await self.fetch("big.csv", seconds: 5) }),
            ("sheet-fetch", { await self.fetch("sheet.xlsx", seconds: 4) }),
            ("zip-entry-churn", { await self.entryChurn("many.zip", rounds: 60) }),
            ("file-churn", { await self.fileChurn(rounds: 3) }),
            ("resize-churn", { await self.resizeChurn("geotopo.pdf", times: 60) }),
            ("libgui-pdf-scroll", { await self.scroll("geotopo.pdf", seconds: 10, speed: 40, libgui: true) }),
            ("libgui-idle", { await self.idle("geotopo.pdf", seconds: 8, libgui: true) }),
        ]
    }

    func run() async {
        Stress.emit(["event": "start", "platform": platform(), "scenarios": names])
        let all = scenarios
        let chosen = names.contains("all") ? all : all.filter { names.contains($0.0) }
        for (name, body) in chosen {
            current = name
            await body()
        }
        Stress.emit(["event": "done"])
        shell.save()
        exit(0)
    }

    private var current = ""

    // MARK: the kinds of load

    /// Nothing happening, the file on screen: what's left is what idles badly
    /// (frames that needn't be drawn, timers, wakeups).
    private func idle(_ file: String, seconds: Double, libgui: Bool = false) async {
        guard await open(file, libgui: libgui) else { return }
        let m = Measure()
        await sleep(seconds)
        report(m, file)
    }

    /// A steady scroll (trackpad, two fingers), down for most of it, then back up.
    private func scroll(_ file: String, view: String? = nil, seconds: Double, speed: Float, libgui: Bool = false) async {
        guard await open(file, view: view, libgui: libgui) else { return }
        let (x, y) = center
        let m = Measure()
        await drive(seconds) { t in
            self.canvas?.pointerMoved(x: x, y: y)
            self.canvas?.wheel(dx: 0, dy: t < seconds * 0.8 ? -speed : speed * 2, precise: true)
        }
        report(m, file)
    }

    /// Finger flings on a touch screen: quick drags and lets go, and the coasting after.
    private func flingTouch(_ file: String, seconds: Double) async {
        guard await open(file) else { return }
        let (x, y) = center
        let m = Measure()
        var id: UInt64 = 1000
        await drive(seconds) { t in
            // A 0.3 s flick up every second; the rest of the second it coasts.
            let phase = t.truncatingRemainder(dividingBy: 1)
            guard let c = self.canvas else { return }
            if phase < 0.02 {
                id += 1
                c.touchPoint(id: id, phase: .start, x: x, y: y + 150)
            } else if phase < 0.3 {
                c.touchPoint(id: id, phase: .move, x: x, y: y + 150 - Float(phase / 0.3) * 300)
            } else if phase < 0.32 {
                c.touchPoint(id: id, phase: .end, x: x, y: y - 150)
            }
        }
        report(m, file)
    }

    /// Pinching in and out over the page, as a trackpad does.
    /// `rest`: seconds after, measured too, so work the zoom only put off (tiles
    /// queued, uploads) counts against it.
    private func zoom(_ file: String, seconds: Double, rest: Double = 0) async {
        guard await open(file) else { return }
        let (x, y) = center
        let m = Measure()
        await drive(seconds) { t in
            self.canvas?.pointerMoved(x: x, y: y)
            self.canvas?.pinch(delta: Float(sin(t * 2.5)) * 0.04)
        }
        if rest > 0 { await sleep(rest) }
        report(m, file)
    }

    /// A key pressed again and again (Page Down: HID usage 0x4E).
    private func keys(_ file: String, seconds: Double, usage: UInt16) async {
        guard await open(file) else { return }
        let m = Measure()
        var down = false
        await drive(seconds) { t in
            let want = t.truncatingRemainder(dividingBy: 0.15) < 0.075
            if want != down {
                down = want
                _ = self.canvas?.keyHid(usage: usage, pressed: down, repeat: false)
            }
        }
        report(m, file)
    }

    /// An image zoomed in and dragged around.
    private func zoomPan(_ file: String, seconds: Double) async {
        guard await open(file) else { return }
        let (x, y) = center
        let m = Measure()
        var pressed = false
        await drive(seconds) { t in
            guard let c = self.canvas else { return }
            if t < 2 {
                c.pointerMoved(x: x, y: y)
                c.pinch(delta: 0.05)
            } else {
                let px = x + Float(cos(t * 2)) * 120, py = y + Float(sin(t * 2)) * 120
                if !pressed {
                    pressed = true
                    c.pointerButton(x: px, y: py, button: .primary, pressed: true)
                }
                c.pointerMoved(x: px, y: py)
            }
        }
        canvas?.pointerButton(x: x, y: y, button: .primary, pressed: false)
        report(m, file)
    }

    /// A model dragged round and round (orbit).
    private func orbit(_ file: String, seconds: Double) async {
        guard await open(file, timeout: 60) else { return }
        let (x, y) = center
        let m = Measure()
        canvas?.pointerButton(x: x, y: y, button: .primary, pressed: true)
        await drive(seconds) { t in
            self.canvas?.pointerMoved(x: x + Float(cos(t * 1.5)) * 150, y: y + Float(sin(t * 3)) * 60)
        }
        canvas?.pointerButton(x: x, y: y, button: .primary, pressed: false)
        report(m, file)
    }

    /// The native lists' cost: rows asked for as a fast scroll would, a screenful
    /// (64 rows) at a time at random places.
    private func fetch(_ file: String, seconds: Double) async {
        guard await open(file) else { return }
        let s = shell.session
        let kind = shell.kind
        let m = Measure()
        var calls = 0
        var worst = 0.0
        let end = CACurrentMediaTime() + seconds
        var rng = SystemRandomNumberGenerator()
        while CACurrentMediaTime() < end {
            let t0 = CACurrentMediaTime()
            switch kind {
            case .text:
                let n = s.textLineCount()
                if n > 0 { _ = s.textLines(start: UInt64.random(in: 0..<n, using: &rng), count: 64) }
            case .table:
                let n = s.tableRowCount()
                if n > 0 { _ = s.tableRows(start: UInt64.random(in: 0..<n, using: &rng), count: 64) }
            default:
                break
            }
            worst = max(worst, (CACurrentMediaTime() - t0) * 1000)
            calls += 1
            if calls % 20 == 0 { await Task.yield() }
        }
        m.extra = ["fetches": calls, "fetch_us_avg": Int(seconds * 1e6 / Double(max(calls, 1))), "fetch_max_ms": round2(worst), "kind": "\(kind)"]
        report(m, file)
    }

    /// Into an archive's entry and back, again and again: what each round leaves
    /// behind shows as memory that only grows.
    private func entryChurn(_ file: String, rounds: Int) async {
        guard await open(file) else { return }
        let m = Measure()
        var mem: [Double] = []
        for i in 0..<rounds {
            shell.openEntry(UInt64(i * 37 % 5000))
            await waitShown(timeout: 10)
            await sleep(0.05)
            shell.back()
            await sleep(0.05)
            if i % 10 == 9 { mem.append(Double(Metrics.footprintBytes()) / 1_048_576) }
        }
        m.extra = ["rounds": rounds, "mem_every_10_rounds_mb": mem.map(round1)]
        report(m, file)
    }

    /// Every kind of file, opened one after another, three times over: memory after
    /// each round should level off, and each open's time is the user's wait.
    private func fileChurn(rounds: Int) async {
        let files = ["geotopo.pdf", "huge.png", "dragon.obj", "mosquito.glb", "document.docx", "slides.pptx",
                     "changelog.md", "big.json", "big.csv", "big.log", "sheet.xlsx", "many.zip", "photo.jpg", "helmet.usdz"]
        let m = Measure()
        var mem: [Double] = []
        var opens: [String: Double] = [:]
        for _ in 0..<rounds {
            for f in files {
                let t0 = CACurrentMediaTime()
                guard await open(f, settle: 0.3) else { continue }
                opens[f] = max(opens[f] ?? 0, (CACurrentMediaTime() - t0 - 0.3) * 1000)
            }
            mem.append(Double(Metrics.footprintBytes()) / 1_048_576)
        }
        m.extra = ["rounds": rounds, "mem_after_each_round_mb": mem.map(round1), "open_ms_worst": opens.mapValues { Int($0) }]
        report(m, nil)
    }

    /// The window resized (or the device turned) again and again.
    private func resizeChurn(_ file: String, times: Int) async {
        guard await open(file) else { return }
        guard let target, let canvas else { return }
        let size = target.stressSize
        let scale = Float(scaleFactor())
        let m = Measure()
        for i in 0..<times {
            let f: CGFloat = i % 2 == 0 ? 0.6 : 1
            canvas.resize(width: UInt32(size.width * f * CGFloat(scale)), height: UInt32(size.height * CGFloat(scale)), scale: scale)
            target.redraw()
            await sleep(1.0 / 30)
        }
        canvas.resize(width: UInt32(size.width * CGFloat(scale)), height: UInt32(size.height * CGFloat(scale)), scale: scale)
        target.redraw()
        await sleep(0.5)
        m.extra = ["resizes": times]
        report(m, file)
    }

    // MARK: plumbing

    /// Opens a Documents file (in the view or interface asked for) and waits until it
    /// is shown and settled. False (and a line saying why) if it never was.
    private func open(_ file: String, view: String? = nil, libgui: Bool = false, timeout: Double = 30, settle: Double = 1) async -> Bool {
        let want: InterfaceChoice = libgui ? .libgui : .native
        if shell.settings.interface != want {
            shell.settings.interface = want
            await sleep(0.5)
        }
        let t0 = CACurrentMediaTime()
        shell.open(docs.appendingPathComponent(file))
        guard await waitShown(timeout: timeout) else {
            Stress.emit(["scenario": current, "file": file, "error": shell.error ?? "not shown in \(Int(timeout)) s"])
            return false
        }
        let openMs = (CACurrentMediaTime() - t0) * 1000
        if let view, let i = shell.choices.names.firstIndex(of: view) {
            shell.choose(i)
            _ = await waitShown(timeout: timeout)
        }
        await sleep(settle)
        lastOpenMs = openMs
        return true
    }

    private var lastOpenMs = 0.0

    @discardableResult
    private func waitShown(timeout: Double) async -> Bool {
        let end = CACurrentMediaTime() + timeout
        while CACurrentMediaTime() < end {
            if shell.error != nil { return false }
            if case .shown = shell.status { return true }
            await sleep(0.02)
        }
        return false
    }

    /// Calls `step` every 1/60 s for `seconds`, with the time since the start, and
    /// asks for a frame each time (as input does).
    private func drive(_ seconds: Double, step: @escaping (Double) -> Void) async {
        let start = CACurrentMediaTime()
        while true {
            let t = CACurrentMediaTime() - start
            if t >= seconds { break }
            step(t)
            target?.redraw()
            await sleep(1.0 / 60)
        }
        // Let what the input started (a fling, tiles) finish inside the measure.
        await sleep(0.5)
    }

    private func report(_ m: Measure, _ file: String?) {
        var line = m.finish(current, file: file)
        line["open_ms"] = Int(lastOpenMs)
        line["interface"] = shell.settings.interface == .libgui ? "libgui" : "native"
        Stress.emit(line)
    }

    private func sleep(_ seconds: Double) async {
        try? await Task.sleep(nanoseconds: UInt64(seconds * 1e9))
    }

    private func platform() -> String {
        #if os(macOS)
        return "macOS"
        #else
        return "iOS"
        #endif
    }

    private func scaleFactor() -> CGFloat {
        #if os(macOS)
        return NSScreen.main?.backingScaleFactor ?? 2
        #else
        return UIScreen.main.scale
        #endif
    }
}
