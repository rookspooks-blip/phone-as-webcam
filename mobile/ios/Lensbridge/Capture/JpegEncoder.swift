import CoreImage
import ImageIO
import CoreVideo

/// JPEG frames for the optional browser MJPEG stream (port 4747).
final class JpegEncoder {
    private let context = CIContext(options: [.useSoftwareRenderer: false])
    private let colorSpace = CGColorSpace(name: CGColorSpace.sRGB)!

    func encode(_ pb: CVPixelBuffer, maxWidth: CGFloat = 1280, quality: CGFloat = 0.7) -> Data? {
        var img = CIImage(cvPixelBuffer: pb)
        let w = img.extent.width
        if w > maxWidth {
            let s = maxWidth / w
            img = img.transformed(by: CGAffineTransform(scaleX: s, y: s))
        }
        let key = CIImageRepresentationOption(rawValue: kCGImageDestinationLossyCompressionQuality as String)
        return context.jpegRepresentation(of: img, colorSpace: colorSpace, options: [key: quality])
    }
}
