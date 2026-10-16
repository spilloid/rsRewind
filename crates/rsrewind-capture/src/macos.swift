import AppKit
import ScreenCaptureKit
import CoreGraphics

// This bridge only reports what macOS says. Every decision about it (which windows count, when the
// list or the session is unknown) is made in Rust, in `macos_facts.rs`, where it is unit-tested.
// Nothing here logs, and no error code carries window content.

private func json(_ value: Any) -> UnsafeMutablePointer<CChar>? {
    guard JSONSerialization.isValidJSONObject(value),
          let data = try? JSONSerialization.data(withJSONObject: value),
          let s = String(data: data, encoding: .utf8) else { return nil }
    return strdup(s)
}
@_cdecl("rs_capture_free") public func release(_ p: UnsafeMutableRawPointer?) { free(p) }
@_cdecl("rs_capture_permission") public func permission(_ request: Bool) -> Bool {
    return CGPreflightScreenCaptureAccess() || (request && CGRequestScreenCaptureAccess())
}
private struct Display {
    let id: CGDirectDisplayID
    let bounds: CGRect
    let x: Int
    let width: Int
    let height: Int
    var sx: Double { Double(width) / bounds.width }
    var sy: Double { Double(height) / bounds.height }
}
private func displays() -> [Display]? {
    var count: UInt32 = 0
    guard CGGetActiveDisplayList(0, nil, &count) == .success, count > 0 else { return nil }
    var ids = [CGDirectDisplayID](repeating: 0, count: Int(count))
    guard CGGetActiveDisplayList(count, &ids, &count) == .success else { return nil }
    var x = 0
    return ids.prefix(Int(count)).map { id in
        let d = Display(id: id, bounds: CGDisplayBounds(id), x: x, width: CGDisplayPixelsWide(id), height: CGDisplayPixelsHigh(id))
        x += d.width
        return d
    }
}
/// Every display has a finite, positive size in points and pixels. `Display.sx` divides by the
/// point width, and `Int(...)` of an infinite or NaN value traps, so nothing may use a display
/// before this holds.
private func geometryUsable(_ ds: [Display]) -> Bool {
    return ds.allSatisfy { d in
        d.width > 0 && d.height > 0
            && d.bounds.minX.isFinite && d.bounds.minY.isFinite
            && d.bounds.width.isFinite && d.bounds.height.isFinite
            && d.bounds.width > 0 && d.bounds.height > 0
    }
}
private func displayFacts(_ ds: [Display]) -> [[String: Any]] {
    return ds.map { d -> [String: Any] in
        return ["x": d.x, "min_x": Double(d.bounds.minX), "min_y": Double(d.bounds.minY),
                "width_pt": Double(d.bounds.width), "height_pt": Double(d.bounds.height),
                "width_px": d.width, "height_px": d.height]
    }
}
@_cdecl("rs_capture_monitors") public func monitors() -> UnsafeMutablePointer<CChar>? {
    guard let ds = displays() else { return nil }
    guard geometryUsable(ds) else { return json(["error": "bad-display-geometry"]) }
    return json(ds.map { d -> [String: Any] in
        let uuid = CGDisplayCreateUUIDFromDisplayID(d.id).takeRetainedValue()
        return ["id": d.id, "info": ["device_name": "macos-" + (CFUUIDCreateString(nil, uuid) as String), "left": d.x, "top": 0, "width": d.width, "height": d.height, "dpi": Int(96 * d.sx), "primary": d.id == CGMainDisplayID()]]
    })
}
private func ownerPid(_ w: [String: Any]) -> Int32? {
    guard let pid = w[kCGWindowOwnerPID as String] as? Int else { return nil }
    return Int32(exactly: pid)
}
private func boundsOf(_ w: [String: Any]) -> CGRect? {
    guard let b = w[kCGWindowBounds as String] as? [String: Any] else { return nil }
    return CGRect(dictionaryRepresentation: b as CFDictionary)
}
/// Raw facts about the displays and every on-screen window, frontmost first, or `{"error": code}`.
/// Keys are omitted when macOS does not provide them.
@_cdecl("rs_capture_windows") public func windows() -> UnsafeMutablePointer<CChar>? {
    guard CGPreflightScreenCaptureAccess() else { return json(["error": "no-permission"]) }
    guard let ds = displays() else { return json(["error": "no-displays"]) }
    guard geometryUsable(ds) else { return json(["error": "bad-display-geometry"]) }
    guard let list = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID) as? [[String: Any]] else {
        return json(["error": "list-unavailable"])
    }
    var out: [[String: Any]] = []
    out.reserveCapacity(list.count)
    for w in list {
        var f: [String: Any] = [:]
        if let pid = ownerPid(w) {
            f["pid"] = Int(pid)
            if let app = NSRunningApplication(processIdentifier: pid) {
                if let exe = app.executableURL?.lastPathComponent { f["executable"] = exe }
                f["regular"] = app.activationPolicy == .regular
            }
        }
        if let owner = w[kCGWindowOwnerName as String] as? String { f["owner_name"] = owner }
        if let title = w[kCGWindowName as String] as? String { f["title"] = title }
        if let layer = w[kCGWindowLayer as String] as? Int { f["layer"] = layer }
        // Absent when missing, unparseable or not finite: Rust then treats the list as unknown.
        if let b = boundsFacts(w) { f["bounds"] = b }
        out.append(f)
    }
    let payload: [String: Any] = ["displays": displayFacts(ds), "windows": out]
    return json(payload)
}
private func boundsFacts(_ w: [String: Any]) -> [String: Double]? {
    guard let r = boundsOf(w), r.minX.isFinite, r.minY.isFinite, r.width.isFinite, r.height.isFinite else { return nil }
    return ["x": Double(r.minX), "y": Double(r.minY), "width": Double(r.width), "height": Double(r.height)]
}
/// Raw session facts: console/login/lock state, loginwindow windows on screen, idle seconds.
@_cdecl("rs_capture_session") public func session() -> UnsafeMutablePointer<CChar>? {
    var f: [String: Any] = [:]
    if let s = CGSessionCopyCurrentDictionary() as? [String: Any] {
        f["have_session"] = true
        if let v = s[kCGSessionOnConsoleKey as String] as? Bool { f["on_console"] = v }
        if let v = s[kCGSessionLoginDoneKey as String] as? Bool { f["login_done"] = v }
        if let v = s["CGSSessionScreenIsLocked"] as? Bool { f["screen_locked"] = v }
    }
    // All layers: the lock and login UI run as loginwindow.
    if let list = CGWindowListCopyWindowInfo(.optionOnScreenOnly, kCGNullWindowID) as? [[String: Any]] {
        f["windows_listed"] = true
        var login: [[String: Any]] = []
        var anonymous: [[String: Any]] = []
        var unresolved = 0
        for w in list {
            var exe: String? = nil
            if let pid = ownerPid(w), let app = NSRunningApplication(processIdentifier: pid) {
                exe = app.executableURL?.lastPathComponent
            }
            if exe == nil { unresolved += 1 }
            let owner = w[kCGWindowOwnerName as String] as? String
            // Nothing names this window: it could be a lock screen. Rust judges its coverage.
            if (exe ?? "").isEmpty && (owner ?? "").isEmpty {
                var a: [String: Any] = [:]
                if let b = boundsFacts(w) { a["bounds"] = b }
                anonymous.append(a)
            }
            if exe == "loginwindow" || owner == "loginwindow" {
                var l: [String: Any] = [:]
                if let layer = w[kCGWindowLayer as String] as? Int { l["layer"] = layer }
                if let alpha = w[kCGWindowAlpha as String] as? Double, alpha.isFinite { l["alpha"] = alpha }
                if let r = boundsOf(w), r.width.isFinite, r.height.isFinite {
                    l["width"] = Double(r.width); l["height"] = Double(r.height)
                }
                login.append(l)
            }
        }
        f["loginwindow"] = login
        f["anonymous"] = anonymous
        f["unresolved_owners"] = unresolved
    }
    // Geometry to judge unidentified windows against; omitted (Rust then clears nothing) when
    // unusable.
    if let ds = displays(), geometryUsable(ds) { f["displays"] = displayFacts(ds) }
    if let anyInput = CGEventType(rawValue: UInt32.max) {
        let seconds = CGEventSource.secondsSinceLastEventType(.combinedSessionState, eventType: anyInput)
        if seconds.isFinite && seconds >= 0 { f["idle_seconds"] = seconds }
    }
    return json(f)
}
private final class FrameResult: @unchecked Sendable {
    let lock = NSLock()
    var image: CGImage?
    var reason: Int32 = 0
    var code: Int = 0
    func set(_ i: CGImage) { lock.lock(); image = i; lock.unlock() }
    func fail(_ r: Int32, _ e: Error?) {
        lock.lock()
        reason = r
        if let e = e { code = (e as NSError).code }
        lock.unlock()
    }
    func get() -> (CGImage?, Int32, Int) { lock.lock(); defer { lock.unlock() }; return (image, reason, code) }
}
// Failure codes. They must equal `macos_facts::frame_reason`; a Rust test reads these lines.
private let FRAME_NO_PERMISSION: Int32 = 1
private let FRAME_OS_TOO_OLD: Int32 = 2
private let FRAME_SHAREABLE_CONTENT: Int32 = 3
private let FRAME_DISPLAY_NOT_FOUND: Int32 = 4
private let FRAME_SCREENSHOT: Int32 = 5
private let FRAME_TIMEOUT: Int32 = 6
private let FRAME_NO_IMAGE: Int32 = 7
private let FRAME_BAD_SIZE: Int32 = 8
private let FRAME_DRAW: Int32 = 9
@_cdecl("rs_capture_frame") public func frame(_ id: UInt32, _ width: UnsafeMutablePointer<UInt32>, _ height: UnsafeMutablePointer<UInt32>, _ length: UnsafeMutablePointer<Int>, _ reason: UnsafeMutablePointer<Int32>, _ osCode: UnsafeMutablePointer<Int>) -> UnsafeMutableRawPointer? {
    reason.pointee = 0
    osCode.pointee = 0
    guard #available(macOS 14.0, *) else { reason.pointee = FRAME_OS_TOO_OLD; return nil }
    guard CGPreflightScreenCaptureAccess() else { reason.pointee = FRAME_NO_PERMISSION; return nil }
    let done = DispatchSemaphore(value: 0)
    let result = FrameResult()
    SCShareableContent.getExcludingDesktopWindows(false, onScreenWindowsOnly: true) { content, error in
        guard let content = content else { result.fail(FRAME_SHAREABLE_CONTENT, error); done.signal(); return }
        guard let display = content.displays.first(where: { $0.displayID == id }) else { result.fail(FRAME_DISPLAY_NOT_FOUND, nil); done.signal(); return }
        let config = SCStreamConfiguration()
        config.width = CGDisplayPixelsWide(id)
        config.height = CGDisplayPixelsHigh(id)
        config.showsCursor = true
        let filter = SCContentFilter(display: display, excludingWindows: [])
        SCScreenshotManager.captureImage(contentFilter: filter, configuration: config) { image, error in
            if let image = image { result.set(image) } else { result.fail(FRAME_SCREENSHOT, error) }
            done.signal()
        }
    }
    guard done.wait(timeout: .now() + 15) == .success else { reason.pointee = FRAME_TIMEOUT; return nil }
    let (captured, failure, code) = result.get()
    guard let image = captured else {
        reason.pointee = failure == 0 ? FRAME_NO_IMAGE : failure
        osCode.pointee = code
        return nil
    }
    let bytes = image.width * image.height * 4
    guard bytes > 0, bytes <= 512 * 1024 * 1024, let p = malloc(bytes) else { reason.pointee = FRAME_BAD_SIZE; return nil }
    guard let ctx = CGContext(data: p, width: image.width, height: image.height, bitsPerComponent: 8, bytesPerRow: image.width * 4, space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.premultipliedFirst.rawValue | CGBitmapInfo.byteOrder32Little.rawValue) else { free(p); reason.pointee = FRAME_DRAW; return nil }
    ctx.translateBy(x: 0, y: CGFloat(image.height)); ctx.scaleBy(x: 1, y: -1)
    ctx.draw(image, in: CGRect(x: 0, y: 0, width: image.width, height: image.height))
    width.pointee = UInt32(image.width); height.pointee = UInt32(image.height); length.pointee = bytes
    return p
}
