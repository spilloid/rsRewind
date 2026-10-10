import AppKit
import ScreenCaptureKit
import CoreGraphics

private func json(_ value: Any) -> UnsafeMutablePointer<CChar>? {
    guard let data = try? JSONSerialization.data(withJSONObject: value), let s = String(data: data, encoding: .utf8) else { return nil }
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
@_cdecl("rs_capture_monitors") public func monitors() -> UnsafeMutablePointer<CChar>? {
    guard let ds = displays() else { return nil }
    return json(ds.map { d -> [String: Any] in
        let uuid = CGDisplayCreateUUIDFromDisplayID(d.id).takeRetainedValue()
        return ["id": d.id, "info": ["device_name": "macos-" + (CFUUIDCreateString(nil, uuid) as String), "left": d.x, "top": 0, "width": d.width, "height": d.height, "dpi": Int(96 * d.sx), "primary": d.id == CGMainDisplayID()]]
    })
}
@_cdecl("rs_capture_windows") public func windows() -> UnsafeMutablePointer<CChar>? {
    guard CGPreflightScreenCaptureAccess(), let ds = displays(), let list = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID) as? [[String: Any]] else { return nil }
    var result: [[String: Any]] = []
    for w in list {
        guard w[kCGWindowLayer as String] is Int else { return nil }
        // ScreenCaptureKit captures all layers; include every visible owner, including overlays.
        guard let pid = w[kCGWindowOwnerPID as String] as? Int32,
              let bounds = w[kCGWindowBounds as String] as? CFDictionary,
              let r = CGRect(dictionaryRepresentation: bounds),
              let app = NSRunningApplication(processIdentifier: pid),
              let process = app.executableURL?.lastPathComponent else { return nil }
        let title = w[kCGWindowName as String] as? String
        // A missing title is unknown, not an empty title, including overlays.
        if title == nil { return nil }
        for d in ds {
            let clipped = r.intersection(d.bounds)
            if clipped.isNull || clipped.isEmpty { continue }
            let left = d.x + Int(floor((clipped.minX - d.bounds.minX) * d.sx))
            let top = Int(floor((clipped.minY - d.bounds.minY) * d.sy))
            result.append(["process_name": process, "title": title ?? "", "pid": pid,
                "left": left, "top": top, "right": d.x + Int(ceil((clipped.maxX - d.bounds.minX) * d.sx)), "bottom": Int(ceil((clipped.maxY - d.bounds.minY) * d.sy))])
        }
    }
    return json(result)
}
@_cdecl("rs_capture_idle") public func idle() -> Double {
    guard let session = CGSessionCopyCurrentDictionary() as? [String: Any],
          let console = session[kCGSessionOnConsoleKey as String] as? Bool, console,
          let loggedIn = session[kCGSessionLoginDoneKey as String] as? Bool, loggedIn else { return -1 }
    // Window-server login/lock UI runs as loginwindow. Failure to enumerate is unknown.
    guard let list = CGWindowListCopyWindowInfo(.optionOnScreenOnly, kCGNullWindowID) as? [[String: Any]] else { return -1 }
    for w in list {
        guard let pid = w[kCGWindowOwnerPID as String] as? Int32, let app = NSRunningApplication(processIdentifier: pid), let name = app.executableURL?.lastPathComponent else { return -1 }
        if name == "loginwindow" { return -1 }
    }
    guard let anyInput = CGEventType(rawValue: UInt32.max) else { return -1 }
    let seconds = CGEventSource.secondsSinceLastEventType(.combinedSessionState, eventType: anyInput)
    return seconds.isFinite && seconds >= 0 ? seconds * 1000 : -1
}
private final class ImageResult: @unchecked Sendable {
    let lock = NSLock()
    var image: CGImage?
    func set(_ i: CGImage?) { lock.lock(); image = i; lock.unlock() }
    func get() -> CGImage? { lock.lock(); defer { lock.unlock() }; return image }
}
@_cdecl("rs_capture_frame") public func frame(_ id: UInt32, _ width: UnsafeMutablePointer<UInt32>, _ height: UnsafeMutablePointer<UInt32>, _ length: UnsafeMutablePointer<Int>) -> UnsafeMutableRawPointer? {
    guard #available(macOS 14.0, *), CGPreflightScreenCaptureAccess(), idle() >= 0 else { return nil }
    let done = DispatchSemaphore(value: 0)
    let result = ImageResult()
    SCShareableContent.getExcludingDesktopWindows(false, onScreenWindowsOnly: true) { content, _ in
        guard let display = content?.displays.first(where: { $0.displayID == id }) else { done.signal(); return }
        let config = SCStreamConfiguration()
        config.width = CGDisplayPixelsWide(id)
        config.height = CGDisplayPixelsHigh(id)
        config.showsCursor = true
        let filter = SCContentFilter(display: display, excludingWindows: [])
        SCScreenshotManager.captureImage(contentFilter: filter, configuration: config) { image, _ in
            result.set(image); done.signal()
        }
    }
    guard done.wait(timeout: .now() + 15) == .success, let image = result.get(), idle() >= 0 else { return nil }
    let bytes = image.width * image.height * 4
    guard bytes > 0, bytes <= 512 * 1024 * 1024, let p = malloc(bytes) else { return nil }
    guard let ctx = CGContext(data: p, width: image.width, height: image.height, bitsPerComponent: 8, bytesPerRow: image.width * 4, space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.premultipliedFirst.rawValue | CGBitmapInfo.byteOrder32Little.rawValue) else { free(p); return nil }
    ctx.translateBy(x: 0, y: CGFloat(image.height)); ctx.scaleBy(x: 1, y: -1)
    ctx.draw(image, in: CGRect(x: 0, y: 0, width: image.width, height: image.height))
    width.pointee = UInt32(image.width); height.pointee = UInt32(image.height); length.pointee = bytes
    return p
}
