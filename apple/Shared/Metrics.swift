// What device hardening measures, printed to the console (`xcrun devicectl device
// process launch --console`, or Xcode's): launch to the first drawn frame, each file's
// open, and the memory the system counts against the app (phys_footprint, what iOS
// ends an app for). Cheap: a clock read and one task_info call per event.

import Foundation

enum Metrics {
    /// When the process started (as near as Swift's first use of this gets).
    private static let launch = Date()
    nonisolated(unsafe) private static var drewFirst = false

    /// Starts the clock: called first thing at launch.
    static func start() { _ = launch }

    /// The canvas drew its first frame.
    static func firstFrame() {
        guard !drewFirst else { return }
        drewFirst = true
        log("first frame \(ms(since: launch)) after launch")
    }

    /// A file is on screen: how long it took, and the memory now.
    static func shown(_ name: String, loadMs: Double) {
        log(String(format: "shown %@: loaded in %.0f ms, %@", name, loadMs, footprint()))
        // Again once its textures and models have reached the GPU.
        DispatchQueue.main.asyncAfter(deadline: .now() + 3) { log("  3 s later: \(footprint())") }
    }

    static func log(_ line: String) {
        print("viewer-metrics: \(line)")
        fflush(stdout)
    }

    private static func ms(since t: Date) -> String {
        String(format: "%.0f ms", Date().timeIntervalSince(t) * 1000)
    }

    /// Hands memory the allocator holds free back to the system. Decoders free their
    /// buffers, but the allocator keeps the pages (Sponza at rest: 107 MB live, 429 MB
    /// counted against the app), and iOS ends apps by what's counted. Cheap; done when
    /// a burst of work ends, at most once a second.
    nonisolated(unsafe) private static var lastRelief: CFAbsoluteTime = 0
    static func releaseFreeMemory() {
        let now = CFAbsoluteTimeGetCurrent()
        guard now - lastRelief > 1 else { return }
        lastRelief = now
        malloc_zone_pressure_relief(nil, 0)
    }

    /// phys_footprint, in bytes (0 if unknown).
    static func footprintBytes() -> UInt64 {
        var info = task_vm_info_data_t()
        var count = mach_msg_type_number_t(MemoryLayout<task_vm_info_data_t>.size / MemoryLayout<natural_t>.size)
        let kr = withUnsafeMutablePointer(to: &info) {
            $0.withMemoryRebound(to: integer_t.self, capacity: Int(count)) { task_info(mach_task_self_, task_flavor_t(TASK_VM_INFO), $0, &count) }
        }
        return kr == KERN_SUCCESS ? info.phys_footprint : 0
    }

    /// phys_footprint, in MB.
    static func footprint() -> String {
        var info = task_vm_info_data_t()
        var count = mach_msg_type_number_t(MemoryLayout<task_vm_info_data_t>.size / MemoryLayout<natural_t>.size)
        let kr = withUnsafeMutablePointer(to: &info) {
            $0.withMemoryRebound(to: integer_t.self, capacity: Int(count)) { task_info(mach_task_self_, task_flavor_t(TASK_VM_INFO), $0, &count) }
        }
        guard kr == KERN_SUCCESS else { return "footprint unknown" }
        return String(format: "%.0f MB footprint", Double(info.phys_footprint) / 1_048_576)
    }
}
