#if os(macOS)
import CoreText
import CoreImage
import Foundation

struct TypographySample: Codable { let opacity: Double; let blur: Double; let rise: Double }

final class TypographyService {
  func bloom(progress: Double, startBlur: Double, risePixels: Double) -> TypographySample {
    let p = max(0, min(1, progress)); let sharp = p * p
    return TypographySample(opacity: sharp, blur: max(0, startBlur * (1 - sharp)), rise: -risePixels * p)
  }

  func textImage(_ text: String, fontSize: CGFloat = 64) -> CIImage? {
    let font = CTFontCreateWithName("Helvetica-Bold" as CFString, fontSize, nil)
    let attributed = NSAttributedString(
      string: text,
      attributes: [
        kCTFontAttributeName as NSAttributedString.Key: font,
        kCTForegroundColorAttributeName as NSAttributedString.Key: CGColor(gray: 1, alpha: 1)
      ]
    )
    return CIFilter(
      name: "CIAttributedTextImageGenerator",
      parameters: ["inputText": attributed, "inputScaleFactor": 2]
    )?.outputImage
  }
}
#endif
