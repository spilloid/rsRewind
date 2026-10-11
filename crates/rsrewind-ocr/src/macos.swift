import Foundation
import Vision
import CoreGraphics
@_cdecl("rs_ocr_free") public func release(_ p: UnsafeMutableRawPointer?) { free(p) }
@_cdecl("rs_ocr_recognize") public func recognize(_ pixels: UnsafeRawPointer, _ width: UInt32, _ height: UInt32, _ stride: UInt32, _ language: UnsafePointer<CChar>) -> UnsafeMutablePointer<CChar>? {
    return autoreleasepool {
        let data = Data(bytes: pixels, count: Int(stride) * Int(height))
        guard let provider = CGDataProvider(data: data as CFData), let image = CGImage(width: Int(width), height: Int(height), bitsPerComponent: 8, bitsPerPixel: 32, bytesPerRow: Int(stride), space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGBitmapInfo(rawValue: CGImageAlphaInfo.premultipliedFirst.rawValue | CGBitmapInfo.byteOrder32Little.rawValue), provider: provider, decode: nil, shouldInterpolate: false, intent: .defaultIntent) else { return nil }
        let request = VNRecognizeTextRequest()
        request.recognitionLevel = .accurate
        let lang = String(cString: language)
        if !lang.isEmpty { request.recognitionLanguages = [lang] }
        request.usesLanguageCorrection = true
        do { try VNImageRequestHandler(cgImage: image, orientation: .up).perform([request]) } catch { return nil }
        let blocks = (request.results ?? []).enumerated().compactMap { index, observation -> [String: Any]? in
            guard let text = observation.topCandidates(1).first else { return nil }
            let box = observation.boundingBox
            return ["text": text.string, "x": box.minX * Double(width), "y": (1 - box.maxY) * Double(height), "width": box.width * Double(width), "height": box.height * Double(height), "confidence": text.confidence, "line_index": index]
        }
        guard let encoded = try? JSONSerialization.data(withJSONObject: blocks), let s = String(data: encoded, encoding: .utf8) else { return nil }
        return strdup(s)
    }
}
