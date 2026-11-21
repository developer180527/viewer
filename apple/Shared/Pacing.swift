// The canvas's frame rate: up to 120 Hz on a ProMotion screen while the user's hands
// are on it (dragging, scrolling, pinching, and a fling coasting after), 60 otherwise
// (models loading, animations). A frame drawn twice as often costs about twice the
// GPU, so the fast rate is only for when it shows.

import QuartzCore

@MainActor
final class Pacing {
    /// How long after the last input the fast rate stays: a fling's coast.
    private let linger: CFTimeInterval = 1.0
    private var lastInput: CFTimeInterval = 0
    /// Whether the fast rate is set; `nil` before the first frame.
    private var fast: Bool?

    private static let quick = CAFrameRateRange(minimum: 80, maximum: 120, preferred: 120)
    private static let normal = CAFrameRateRange(minimum: 30, maximum: 60, preferred: 60)

    /// Input arrived.
    func input() { lastInput = CACurrentMediaTime() }

    /// Sets `link`'s rate for now, if it changed.
    func apply(to link: CADisplayLink?) {
        guard let link else { return }
        let want = CACurrentMediaTime() - lastInput < linger
        if fast == want { return }
        fast = want
        link.preferredFrameRateRange = want ? Self.quick : Self.normal
    }
}
