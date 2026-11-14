import SwiftUI
import FileIDShared

struct PipelineTrackGeometry {
    let width: CGFloat
    let stageCount: Int

    var columnWidth: CGFloat {
        guard stageCount > 0 else { return 0 }
        return width / CGFloat(stageCount)
    }

    func center(for index: Int) -> CGFloat {
        guard stageCount > 0 else { return 0 }
        let bounded = min(max(index, 0), stageCount - 1)
        return columnWidth * (CGFloat(bounded) + 0.5)
    }

    func filledWidth(through index: Int) -> CGFloat {
        max(0, center(for: index) - center(for: 0))
    }
}
