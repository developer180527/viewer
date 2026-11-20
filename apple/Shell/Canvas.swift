// The Metal view: a CAMetalLayer that libviewer draws into through a ViewerCanvas, with
// the same renderers as the libgui app (pages, images, 3D, formatted text, trees,
// hex). AppKit owns the view and its events; Rust draws.

import AppKit
import QuartzCore
import SwiftUI

struct DrawnView: NSViewRepresentable {
    @EnvironmentObject var shell: Shell

    func makeNSView(context: Context) -> CanvasView {
        let view = CanvasView()
        view.shell = shell
        return view
    }

    func updateNSView(_ view: CanvasView, context: Context) {
        // A new page, view or load: draw it.
        view.redraw()
    }

    static func dismantleNSView(_ view: CanvasView, coordinator: ()) {
        view.close()
    }
}

final class CanvasView: NSView {
    weak var shell: Shell?
    private var canvas: ViewerCanvas?
    private var failed = false
    private var link: CADisplayLink?
    private var timer: Timer?
    private var tracking: NSTrackingArea?
    private var cursor: CursorShape = .arrow

    override init(frame: NSRect) {
        super.init(frame: frame)
        wantsLayer = true
        layerContentsRedrawPolicy = .never
    }

    required init?(coder: NSCoder) { fatalError("not from a nib") }

    override func makeBackingLayer() -> CALayer {
        let layer = CAMetalLayer()
        layer.isOpaque = true
        return layer
    }

    // Points from the top left, as libgui's.
    override var isFlipped: Bool { true }
    override var acceptsFirstResponder: Bool { true }
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }

    private var scale: CGFloat { window?.backingScaleFactor ?? 2 }
    private var pixels: (UInt32, UInt32) {
        (UInt32(max(1, bounds.width * scale)), UInt32(max(1, bounds.height * scale)))
    }
    private var dark: Bool { effectiveAppearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua }

    // MARK: lifecycle

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        guard window != nil else { return }
        layer?.contentsScale = scale
        if canvas == nil { start() }
        shell?.onWake = { [weak self] in self?.canvas?.touch(); self?.redraw() }
        redraw()
    }

    private func start() {
        guard !failed, let shell, let layer = layer as? CAMetalLayer, bounds.width > 0 else { return }
        let address = UInt64(UInt(bitPattern: Unmanaged.passUnretained(layer).toOpaque()))
        let (w, h) = pixels
        do {
            canvas = try ViewerCanvas(session: shell.session, layer: address, width: w, height: h, scale: Float(scale), dark: dark)
        } catch {
            failed = true
            shell.canvasFailed("\(error)")
        }
        let link = displayLink(target: self, selector: #selector(tick))
        link.isPaused = true
        link.add(to: .main, forMode: .common)
        self.link = link
    }

    func close() {
        link?.invalidate()
        link = nil
        timer?.invalidate()
        canvas = nil
    }

    override func setFrameSize(_ size: NSSize) {
        super.setFrameSize(size)
        if canvas == nil { start() }
        let (w, h) = pixels
        canvas?.resize(width: w, height: h, scale: Float(scale))
        redraw()
    }

    override func viewDidChangeBackingProperties() {
        super.viewDidChangeBackingProperties()
        layer?.contentsScale = scale
        let (w, h) = pixels
        canvas?.resize(width: w, height: h, scale: Float(scale))
        redraw()
    }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        canvas?.setDark(dark: dark)
        redraw()
    }

    // MARK: frames

    /// A frame on the next display refresh.
    func redraw() {
        timer?.invalidate()
        link?.isPaused = false
    }

    @objc private func tick() {
        guard let canvas else { link?.isPaused = true; return }
        let frame = canvas.draw()
        if frame.opened { shell?.refresh() }
        if frame.cursor != cursor {
            cursor = frame.cursor
            window?.invalidateCursorRects(for: self)
            nsCursor(cursor).set()
        }
        switch frame.repaintAfter {
        case let .some(t) where t <= 0:
            break // keep the display link running: an animation, a drag, a model loading
        case let .some(t):
            link?.isPaused = true
            timer = Timer.scheduledTimer(withTimeInterval: t, repeats: false) { [weak self] _ in
                MainActor.assumeIsolated { self?.redraw() }
            }
        case .none:
            link?.isPaused = true
        }
    }

    // MARK: input

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        if let tracking { removeTrackingArea(tracking) }
        let area = NSTrackingArea(rect: bounds, options: [.mouseMoved, .mouseEnteredAndExited, .activeInKeyWindow, .inVisibleRect], owner: self)
        addTrackingArea(area)
        tracking = area
    }

    override func resetCursorRects() {
        addCursorRect(bounds, cursor: nsCursor(cursor))
    }

    private func point(_ event: NSEvent) -> (Float, Float) {
        let p = convert(event.locationInWindow, from: nil)
        return (Float(p.x), Float(p.y))
    }

    private func mods(_ event: NSEvent) {
        let f = event.modifierFlags
        canvas?.modifiers(shift: f.contains(.shift), ctrl: f.contains(.control), alt: f.contains(.option), command: f.contains(.command))
    }

    private func moved(_ event: NSEvent) {
        let (x, y) = point(event)
        canvas?.pointerMoved(x: x, y: y)
        redraw()
    }

    private func button(_ event: NSEvent, _ button: MouseButton, _ pressed: Bool) {
        mods(event)
        let (x, y) = point(event)
        canvas?.pointerButton(x: x, y: y, button: button, pressed: pressed)
        redraw()
    }

    override func mouseMoved(with event: NSEvent) { moved(event) }
    override func mouseDragged(with event: NSEvent) { moved(event) }
    override func rightMouseDragged(with event: NSEvent) { moved(event) }
    override func otherMouseDragged(with event: NSEvent) { moved(event) }
    override func mouseExited(with event: NSEvent) { canvas?.pointerLeft(); redraw() }

    override func mouseDown(with event: NSEvent) {
        window?.makeFirstResponder(self)
        button(event, .primary, true)
    }
    override func mouseUp(with event: NSEvent) { button(event, .primary, false) }
    override func rightMouseDown(with event: NSEvent) { button(event, .secondary, true) }
    override func rightMouseUp(with event: NSEvent) { button(event, .secondary, false) }
    override func otherMouseDown(with event: NSEvent) { button(event, .middle, true) }
    override func otherMouseUp(with event: NSEvent) { button(event, .middle, false) }

    override func scrollWheel(with event: NSEvent) {
        mods(event)
        let (x, y) = point(event)
        canvas?.pointerMoved(x: x, y: y)
        canvas?.wheel(dx: Float(event.scrollingDeltaX), dy: Float(event.scrollingDeltaY), precise: event.hasPreciseScrollingDeltas)
        redraw()
    }

    override func magnify(with event: NSEvent) {
        let (x, y) = point(event)
        canvas?.pointerMoved(x: x, y: y)
        canvas?.pinch(delta: Float(event.magnification))
        redraw()
    }

    override func flagsChanged(with event: NSEvent) { mods(event); redraw() }

    override func resignFirstResponder() -> Bool {
        canvas?.focusLost()
        redraw()
        return true
    }
}

private func nsCursor(_ shape: CursorShape) -> NSCursor {
    switch shape {
    case .arrow: return .arrow
    case .pointer: return .pointingHand
    case .resizeHorizontal: return .resizeLeftRight
    case .resizeVertical: return .resizeUpDown
    case .resizeDiagonal: return .crosshair
    case .grab: return .openHand
    case .grabbing: return .closedHand
    case .text: return .iBeam
    case .crosshair: return .crosshair
    }
}
