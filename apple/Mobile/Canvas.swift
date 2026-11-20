// The Metal view on iPhone and iPad: a CAMetalLayer that libviewer draws into through a
// ViewerCanvas, as on the Mac (Mac/Canvas.swift). Touches go to libgui as they are, so
// its scrolling, flings and two-finger pinch are the libgui app's; an iPad's keyboard
// sends keys, and its trackpad or mouse hovers, clicks and scrolls.

import QuartzCore
import SwiftUI
import UIKit

struct DrawnView: UIViewRepresentable {
    @EnvironmentObject var shell: Shell
    /// libgui's whole interface (the Interface setting), not just the page.
    var whole = false

    func makeUIView(context: Context) -> CanvasView {
        let view = CanvasView()
        view.shell = shell
        view.whole = whole
        return view
    }

    func updateUIView(_ view: CanvasView, context: Context) {
        view.redraw()
    }

    static func dismantleUIView(_ view: CanvasView, coordinator: ()) {
        view.close()
    }
}

final class CanvasView: UIView {
    weak var shell: Shell?
    /// Draws libgui's whole interface on the shared session, not just the page.
    var whole = false
    private var canvas: ViewerCanvas?
    private var failed = false
    private var link: CADisplayLink?
    private var timer: Timer?
    /// Touches by the number libgui knows them by.
    private var fingers: [ObjectIdentifier: UInt64] = [:]
    private var nextFinger: UInt64 = 1

    override class var layerClass: AnyClass { CAMetalLayer.self }

    override init(frame: CGRect) {
        super.init(frame: frame)
        isMultipleTouchEnabled = true
        isOpaque = true
        // An iPad's trackpad or mouse: hover moves the pointer, two-finger scrolls
        // arrive as a pan with no touches.
        addGestureRecognizer(UIHoverGestureRecognizer(target: self, action: #selector(hover)))
        let scroll = UIPanGestureRecognizer(target: self, action: #selector(scrolled))
        scroll.allowedScrollTypesMask = .all
        scroll.allowedTouchTypes = []
        addGestureRecognizer(scroll)
        registerForTraitChanges([UITraitUserInterfaceStyle.self]) { (self: CanvasView, _: UITraitCollection) in
            self.canvas?.setDark(dark: self.dark)
            self.redraw()
        }
    }

    required init?(coder: NSCoder) { fatalError("not from a nib") }

    private var scale: CGFloat { window?.screen.scale ?? traitCollection.displayScale }
    private var pixels: (UInt32, UInt32) {
        (UInt32(max(1, bounds.width * scale)), UInt32(max(1, bounds.height * scale)))
    }
    private var dark: Bool { traitCollection.userInterfaceStyle == .dark }

    // MARK: lifecycle

    override func didMoveToWindow() {
        super.didMoveToWindow()
        guard window != nil else { return }
        layer.contentsScale = scale
        if canvas == nil { start() }
        shell?.onWake = { [weak self] in self?.canvas?.touch(); self?.redraw() }
        becomeFirstResponder()
        redraw()
    }

    private func start() {
        guard !failed, canvas == nil, let shell, let layer = layer as? CAMetalLayer, bounds.width > 0 else { return }
        let address = UInt64(UInt(bitPattern: Unmanaged.passUnretained(layer).toOpaque()))
        let (w, h) = pixels
        do {
            canvas = whole
                ? try ViewerCanvas.app(session: shell.session, layer: address, width: w, height: h, scale: Float(scale), dark: dark)
                : try ViewerCanvas(session: shell.session, layer: address, width: w, height: h, scale: Float(scale), dark: dark)
            canvas?.setTouchScreen(touch: true)
            let s = safeAreaInsets
            canvas?.setSafeArea(top: Float(s.top), left: Float(s.left), bottom: Float(s.bottom), right: Float(s.right))
        } catch {
            failed = true
            shell.canvasFailed("\(error)")
            return
        }
        shell.stressTarget = self
        let link = CADisplayLink(target: self, selector: #selector(tick))
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

    override func safeAreaInsetsDidChange() {
        super.safeAreaInsetsDidChange()
        let s = safeAreaInsets
        canvas?.setSafeArea(top: Float(s.top), left: Float(s.left), bottom: Float(s.bottom), right: Float(s.right))
        redraw()
    }

    override func layoutSubviews() {
        super.layoutSubviews()
        layer.contentsScale = scale
        if canvas == nil { start() }
        let (w, h) = pixels
        canvas?.resize(width: w, height: h, scale: Float(scale))
        redraw()
    }


    // MARK: frames

    func redraw() {
        timer?.invalidate()
        link?.isPaused = false
    }

    @objc private func tick() {
        guard let canvas else { link?.isPaused = true; return }
        let t0 = CACurrentMediaTime()
        let frame = canvas.draw()
        Stress.frame((CACurrentMediaTime() - t0) * 1000)
        Metrics.firstFrame()
        if frame.opened { shell?.refresh() }
        if let request = frame.request {
            // After this frame: a picker shouldn't come up inside the display link.
            DispatchQueue.main.async { [weak self] in
                guard let shell = self?.shell else { return }
                switch request {
                case .open: shell.openPanel()
                case let .allowFolder(reopen, folder): shell.allow(reopen: reopen, folder: folder)
                }
            }
        }
        switch frame.repaintAfter {
        case let .some(t) where t <= 0:
            break // an animation, a fling, a model loading: every refresh
        case let .some(t):
            link?.isPaused = true
            timer = Timer.scheduledTimer(withTimeInterval: t, repeats: false) { [weak self] _ in
                MainActor.assumeIsolated { self?.redraw() }
            }
        case .none:
            link?.isPaused = true
        }
    }

    // MARK: accessibility

    // One element: VoiceOver reads what's shown, and lets the fingers through (direct
    // interaction), so a page can still be dragged and pinched.
    override var isAccessibilityElement: Bool {
        get { true }
        set {}
    }
    override var accessibilityLabel: String? {
        get { whole ? Shell.libguiAccessibilityLabel : shell?.drawnLabel }
        set {}
    }
    override var accessibilityTraits: UIAccessibilityTraits {
        get { whole ? [.allowsDirectInteraction] : [.image, .allowsDirectInteraction] }
        set {}
    }
    override var accessibilityValue: String? {
        get { whole ? nil : shell?.drawnValue }
        set {}
    }
    override var accessibilityHint: String? {
        get { whole ? nil : "Drag to move, pinch to zoom." }
        set {}
    }

    // MARK: touches

    private func send(_ touches: Set<UITouch>, _ phase: TouchPhase) {
        guard let canvas else { return }
        for t in touches {
            let key = ObjectIdentifier(t)
            let id: UInt64
            if let known = fingers[key] {
                id = known
            } else {
                id = nextFinger
                nextFinger += 1
                fingers[key] = id
            }
            let p = t.location(in: self)
            if t.type == .indirectPointer {
                // An iPad trackpad or mouse click: a button, not a finger.
                canvas.pointerButton(x: Float(p.x), y: Float(p.y), button: .primary, pressed: phase == .start || phase == .move)
            } else {
                canvas.touchPoint(id: id, phase: phase, x: Float(p.x), y: Float(p.y))
            }
            if phase == .end || phase == .cancel { fingers.removeValue(forKey: key) }
        }
        redraw()
    }

    override func touchesBegan(_ touches: Set<UITouch>, with event: UIEvent?) { send(touches, .start) }
    override func touchesMoved(_ touches: Set<UITouch>, with event: UIEvent?) { send(touches, .move) }
    override func touchesEnded(_ touches: Set<UITouch>, with event: UIEvent?) { send(touches, .end) }
    override func touchesCancelled(_ touches: Set<UITouch>, with event: UIEvent?) { send(touches, .cancel) }

    // MARK: pointer and scrolling (iPad)

    @objc private func hover(_ g: UIHoverGestureRecognizer) {
        let p = g.location(in: self)
        switch g.state {
        case .began, .changed: canvas?.pointerMoved(x: Float(p.x), y: Float(p.y))
        default: canvas?.pointerLeft()
        }
        redraw()
    }

    private var lastScroll = CGPoint.zero

    @objc private func scrolled(_ g: UIPanGestureRecognizer) {
        let t = g.translation(in: self)
        if g.state == .began { lastScroll = .zero }
        let p = g.location(in: self)
        canvas?.pointerMoved(x: Float(p.x), y: Float(p.y))
        // As AppKit's precise deltas: content follows the fingers.
        canvas?.wheel(dx: Float(t.x - lastScroll.x), dy: Float(t.y - lastScroll.y), precise: true)
        lastScroll = t
        redraw()
    }

    // MARK: keys (a hardware keyboard)

    override var canBecomeFirstResponder: Bool { true }

    private func keys(_ presses: Set<UIPress>, _ pressed: Bool) -> Bool {
        guard let canvas else { return false }
        var known = false
        for press in presses {
            guard let key = press.key else { continue }
            let f = key.modifierFlags
            canvas.modifiers(shift: f.contains(.shift), ctrl: f.contains(.control), alt: f.contains(.alternate), command: f.contains(.command))
            if canvas.keyHid(usage: UInt16(key.keyCode.rawValue), pressed: pressed, repeat: false) { known = true }
            if pressed, !f.contains(.command), !f.contains(.control), !key.characters.isEmpty,
               let scalar = key.characters.unicodeScalars.first, scalar.value >= 0x20, scalar.value != 0x7F, scalar.value < 0xF700 {
                canvas.text(text: key.characters)
            }
        }
        redraw()
        return known
    }

    override func pressesBegan(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
        if !keys(presses, true) { super.pressesBegan(presses, with: event) }
    }

    override func pressesEnded(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
        if !keys(presses, false) { super.pressesEnded(presses, with: event) }
    }

    override func pressesCancelled(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
        canvas?.focusLost()
        super.pressesCancelled(presses, with: event)
    }
}

extension CanvasView: StressTarget {
    var stressCanvas: ViewerCanvas? { canvas }
    var stressSize: CGSize { bounds.size }
}
