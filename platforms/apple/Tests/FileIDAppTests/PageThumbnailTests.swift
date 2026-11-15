import Testing
import CoreGraphics
@testable import FileID

@Suite struct PageThumbnailTests {
    private func page(width: Int, height: Int, markTop: Bool) -> CGImage {
        let context = CGContext(data: nil, width: width, height: height, bitsPerComponent: 8, bytesPerRow: 0,
                                space: CGColorSpace(name: CGColorSpace.sRGB)!, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
        context.setFillColor(CGColor(red: 1, green: 1, blue: 1, alpha: 1))
        context.fill(CGRect(x: 0, y: 0, width: width, height: height))
        if markTop {
            context.setFillColor(CGColor(red: 0, green: 0, blue: 0, alpha: 1))
            context.fill(CGRect(x: 20, y: height - 40, width: width - 40, height: 6))
        }
        return context.makeImage()!
    }

    @Test func portraitPageShowsItsTop() throws {
        let crop = try #require(PageThumbnail.topCrop(width: 314, height: 512))
        #expect(crop.minY > 0.5 && crop.maxY == 1 && crop.width == 1)
    }

    @Test func landscapeImagesFillTheTile() {
        #expect(PageThumbnail.topCrop(width: 800, height: 600) == nil)
        #expect(PageThumbnail.topCrop(width: 0, height: 0) == nil)
    }

    @Test func blankPageIsDetectedAndTextIsNot() {
        let crop = PageThumbnail.topCrop(width: 314, height: 512)
        #expect(PageThumbnail.isBlank(page(width: 314, height: 512, markTop: false), crop: crop))
        #expect(!PageThumbnail.isBlank(page(width: 314, height: 512, markTop: true), crop: crop))
    }
}

@Suite struct QuickLookRequestTests {
    @Test func requestsStayUnderQuickLooksLimit() {
        #expect(ThumbnailService.quickLookSide(size: 2048, scale: 2) == 1536)
        #expect(ThumbnailService.quickLookSide(size: 512, scale: 2) == 512)
        #expect(ThumbnailService.quickLookSide(size: 2048, scale: 1) == 2048)
        #expect(ThumbnailService.quickLookSide(size: 2048, scale: 0) == 2048)
    }
}

@Suite struct TopLeftCropTests {
    @Test func wideScreenshotsKeepTheirLeftEdge() throws {
        let crop = try #require(PageThumbnail.topLeftCrop(width: 1440, height: 900))
        #expect(crop.minX == 0 && crop.height == 1 && crop.width < 1)
        #expect(PageThumbnail.topLeftCrop(width: 800, height: 600) == nil)
    }

    @Test func onlyScreenshotsAndDocumentImagesQualify() {
        #expect(PageThumbnail.showsTopLeft(kind: "image", hasText: false, tags: [], fileName: "Screenshot 2025-01-10 at 10.20.41.png"))
        #expect(PageThumbnail.showsTopLeft(kind: "image", hasText: true, tags: ["Document", "Page"], fileName: "scan.png"))
        #expect(!PageThumbnail.showsTopLeft(kind: "image", hasText: true, tags: ["Sky"], fileName: "IMG_1.jpg"))
        #expect(!PageThumbnail.showsTopLeft(kind: "pdf", hasText: true, tags: ["Document"], fileName: "a.pdf"))
    }
}
