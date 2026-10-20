import SwiftUI
import AppKit
import FileIDShared

/// Library grid on NSCollectionView: cells are reused as they scroll, so a
/// library of tens of thousands of files costs only the visible tiles.
/// Visuals match `FileTile`.
struct LibraryCollectionGrid: NSViewRepresentable {
    let rows: [FileRow]
    let tagsByFile: [Int64: [String]]
    let selectMode: Bool
    let checkedIDs: Set<Int64>
    let selectedID: Int64?
    let refreshToken: String
    var onSelect: (FileRow) -> Void
    var onOpen: (FileRow) -> Void
    var onToggleCheck: (FileRow) -> Void
    var onFindSimilar: (FileRow) -> Void
    var onNearEnd: () -> Void
    var onClearSelection: () -> Void = {}

    func makeCoordinator() -> Coordinator { Coordinator() }

    func makeNSView(context: Context) -> NSScrollView {
        let layout = AdaptiveGridLayout()
        let collection = TileCollectionView()
        collection.collectionViewLayout = layout
        collection.backgroundColors = [.clear]
        collection.isSelectable = false
        collection.register(LibraryTileItem.self, forItemWithIdentifier: LibraryTileItem.identifier)
        collection.dataSource = context.coordinator
        collection.delegate = context.coordinator
        collection.setAccessibilityLabel("Files")

        let scroll = NSScrollView()
        scroll.documentView = collection
        scroll.hasVerticalScroller = true
        scroll.drawsBackground = false
        scroll.automaticallyAdjustsContentInsets = false
        scroll.contentView.drawsBackground = false
        context.coordinator.collection = collection
        collection.onKey = { [weak coordinator = context.coordinator] key in coordinator?.handle(key) ?? false }
        context.coordinator.apply(self, force: true)
        return scroll
    }

    func updateNSView(_ scroll: NSScrollView, context: Context) {
        context.coordinator.apply(self, force: false)
    }

    @MainActor
    final class Coordinator: NSObject, NSCollectionViewDataSource, NSCollectionViewDelegate {
        weak var collection: NSCollectionView?
        var model: LibraryCollectionGrid?
        private var ids: [Int64] = []
        private var lastToken = ""
        private var lastSelectMode = false
        private var lastChecked: Set<Int64> = []
        private var lastSelected: Int64?
        private var lastRows: [FileRow] = []
        private var lastTags: [Int64: [String]] = [:]
        private var lastClickRowID: Int64?
        /// Keyboard cursor in select mode, where the pane selection is hidden.
        private var cursorID: Int64?

        func apply(_ grid: LibraryCollectionGrid, force: Bool) {
            model = grid
            guard let collection else { return }
            let newIDs = grid.rows.map(\.id)
            let stateChanged = grid.refreshToken != lastToken || grid.selectMode != lastSelectMode
                || grid.checkedIDs != lastChecked || grid.selectedID != lastSelected
            if !grid.selectMode { cursorID = nil }
            if force || newIDs != ids {
                let appended = !ids.isEmpty && newIDs.count > ids.count && Array(newIDs.prefix(ids.count)) == ids
                let old = ids.count
                ids = newIDs
                if appended {
                    let paths = Set((old..<newIDs.count).map { IndexPath(item: $0, section: 0) })
                    NSAnimationContext.runAnimationGroup { $0.duration = 0; collection.insertItems(at: paths) }
                    if stateChanged || grid.tagsByFile != lastTags {
                        reconfigureVisible(grid, reloadImages: grid.refreshToken != lastToken)
                    }
                } else {
                    collection.reloadData()
                }
            } else if stateChanged || grid.rows != lastRows || grid.tagsByFile != lastTags {
                reconfigureVisible(grid, reloadImages: grid.refreshToken != lastToken)
            }
            if let selected = grid.selectedID, selected != lastSelected, let index = newIDs.firstIndex(of: selected) {
                collection.scrollToItems(at: [IndexPath(item: index, section: 0)], scrollPosition: .nearestHorizontalEdge)
            }
            lastToken = grid.refreshToken
            lastSelectMode = grid.selectMode
            lastChecked = grid.checkedIDs
            lastSelected = grid.selectedID
            lastRows = grid.rows
            lastTags = grid.tagsByFile
        }

        private func reconfigureVisible(_ grid: LibraryCollectionGrid, reloadImages: Bool) {
            guard let collection else { return }
            let oldPaths = Dictionary(lastRows.map { ($0.id, $0.pathText) }, uniquingKeysWith: { a, _ in a })
            for item in collection.visibleItems() {
                guard let tile = item as? LibraryTileItem, let path = collection.indexPath(for: item),
                      path.item < grid.rows.count else { continue }
                let row = grid.rows[path.item]
                configure(tile, row: row, reloadImage: reloadImages || oldPaths[row.id] != row.pathText)
            }
        }

        func handle(_ key: TileCollectionView.TileKey) -> Bool {
            guard let model, let collection, !model.rows.isEmpty else { return false }
            let focusID = model.selectMode ? cursorID : model.selectedID
            let current = focusID.flatMap { id in model.rows.firstIndex { $0.id == id } }
            if key == .clear {
                guard current != nil else { return false }
                if model.selectMode { moveCursor(to: nil) } else { model.onClearSelection() }
                return true
            }
            if key == .open {
                guard let current else { return false }
                if model.selectMode { model.onToggleCheck(model.rows[current]) } else { model.onOpen(model.rows[current]) }
                return true
            }
            let columns = max(1, (collection.collectionViewLayout as? AdaptiveGridLayout)?.columnCount ?? 1)
            let start = current ?? -1
            let next: Int = switch key {
            case .left: start - 1
            case .right: start + 1
            case .up: start - columns
            case .down: start < 0 ? 0 : start + columns
            default: start
            }
            let target = min(max(next, 0), model.rows.count - 1)
            guard target != current else { return true }
            if model.selectMode { moveCursor(to: model.rows[target].id) } else { model.onSelect(model.rows[target]) }
            collection.scrollToItems(at: [IndexPath(item: target, section: 0)], scrollPosition: .nearestHorizontalEdge)
            if target >= model.rows.count - 24 { model.onNearEnd() }
            return true
        }

        private func moveCursor(to id: Int64?) {
            cursorID = id
            if let model { reconfigureVisible(model, reloadImages: false) }
        }

        func collectionView(_ collectionView: NSCollectionView, numberOfItemsInSection section: Int) -> Int { ids.count }

        func collectionView(_ collectionView: NSCollectionView, itemForRepresentedObjectAt indexPath: IndexPath) -> NSCollectionViewItem {
            let item = collectionView.makeItem(withIdentifier: LibraryTileItem.identifier, for: indexPath)
            if let tile = item as? LibraryTileItem, let model, indexPath.item < model.rows.count {
                configure(tile, row: model.rows[indexPath.item], reloadImage: true)
            }
            return item
        }

        func collectionView(_ collectionView: NSCollectionView, willDisplay item: NSCollectionViewItem, forRepresentedObjectAt indexPath: IndexPath) {
            if let model, indexPath.item >= model.rows.count - 24 { model.onNearEnd() }
        }

        private func configure(_ tile: LibraryTileItem, row: FileRow, reloadImage: Bool) {
            guard let model else { return }
            tile.configure(
                row: row,
                tags: LibraryTileItem.displayTags(model.tagsByFile[row.id] ?? []),
                selected: model.selectMode ? cursorID == row.id : model.selectedID == row.id,
                selectMode: model.selectMode,
                checked: model.checkedIDs.contains(row.id),
                reloadImage: reloadImage)
            tile.onClick = { [weak self] count in
                guard let self, let model = self.model else { return }
                // A first click can open the details pane and reflow the grid, so the
                // second click of a double-click may land on a different tile.
                let sameTile = self.lastClickRowID == row.id
                self.lastClickRowID = row.id
                if model.selectMode { model.onToggleCheck(row) }
                else if count >= 2 && sameTile { model.onOpen(row) }
                else { model.onSelect(row) }
            }
            tile.onFindSimilar = { [weak self] in self?.model?.onFindSimilar(row) }
            tile.onOpenPreview = { [weak self] in self?.model?.onOpen(row) }
            tile.onRevealInFinder = { NSWorkspace.shared.activateFileViewerSelecting([row.url]) }
        }
    }
}

/// Columns of 160–220 pt with 12 pt gaps and 20 pt insets, like the SwiftUI grid.
final class AdaptiveGridLayout: NSCollectionViewFlowLayout {
    private(set) var columnCount = 1
    override init() {
        super.init()
        minimumInteritemSpacing = 12
        minimumLineSpacing = 16
        sectionInset = NSEdgeInsets(top: 20, left: 20, bottom: 24, right: 20)
    }
    required init?(coder: NSCoder) { fatalError() }

    override func prepare() {
        if let width = collectionView?.enclosingScrollView?.contentView.bounds.width, width > 0 {
            let usable = width - sectionInset.left - sectionInset.right
            let columns = max(1, Int((usable + minimumInteritemSpacing) / (160 + minimumInteritemSpacing)))
            columnCount = columns
            let itemWidth = floor((usable - CGFloat(columns - 1) * minimumInteritemSpacing) / CGFloat(columns))
            itemSize = NSSize(width: itemWidth, height: (itemWidth * 3 / 4).rounded() + LibraryTileItem.captionHeight)
        }
        super.prepare()
    }

    override func shouldInvalidateLayout(forBoundsChange newBounds: NSRect) -> Bool {
        newBounds.width != (collectionView?.bounds.width ?? 0)
    }
}

/// One tracking area for the whole grid; per-tile areas made AppKit rebuild
/// window regions for every cell scrolled into view.
final class TileCollectionView: NSCollectionView {
    /// Arrow keys move the selection, Return/Space open it, Escape clears.
    var onKey: ((TileKey) -> Bool)?
    override var acceptsFirstResponder: Bool { true }
    override var focusRingMaskBounds: NSRect { .zero }

    enum TileKey { case left, right, up, down, open, clear }

    override func keyDown(with event: NSEvent) {
        let key: TileKey? = switch event.keyCode {
        case 123: .left
        case 124: .right
        case 125: .down
        case 126: .up
        case 36, 76, 49: .open
        case 53: .clear
        default: nil
        }
        if let key, onKey?(key) == true { return }
        super.keyDown(with: event)
    }

    override func mouseDown(with event: NSEvent) {
        window?.makeFirstResponder(self)
        super.mouseDown(with: event)
    }
    private var hoverArea: NSTrackingArea?
    private weak var hovered: LibraryTileItem?

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        if let hoverArea { removeTrackingArea(hoverArea) }
        let area = NSTrackingArea(rect: .zero, options: [.mouseMoved, .mouseEnteredAndExited, .activeInKeyWindow, .inVisibleRect], owner: self)
        addTrackingArea(area)
        hoverArea = area
    }

    override func mouseMoved(with event: NSEvent) {
        let point = convert(event.locationInWindow, from: nil)
        let item = indexPathForItem(at: point).flatMap { self.item(at: $0) as? LibraryTileItem }
        guard item !== hovered else { return }
        hovered?.setHover(false)
        item?.setHover(true)
        hovered = item
    }

    override func mouseExited(with event: NSEvent) {
        hovered?.setHover(false)
        hovered = nil
    }
}

/// Single-line text drawn by Core Animation: no Auto Layout, no view redraw.
final class TextLayer: CATextLayer {
    private let textFont: NSFont
    var text: String = "" { didSet { if text != oldValue { string = text } } }
    var textWidth: CGFloat { ceil((text as NSString).size(withAttributes: [.font: textFont]).width) }

    let color: NSColor

    init(size: CGFloat, weight: NSFont.Weight, color: NSColor, align: CATextLayerAlignmentMode = .left,
         truncation: CATextLayerTruncationMode = .end) {
        textFont = LibraryTileItem.brandFont(size: size, weight: weight)
        self.color = color
        super.init()
        font = textFont
        fontSize = size
        foregroundColor = color.cgColor
        alignmentMode = align
        truncationMode = truncation
        isWrapped = false
        contentsScale = NSScreen.main?.backingScaleFactor ?? 2
        actions = ["contents": NSNull(), "bounds": NSNull(), "position": NSNull(), "hidden": NSNull()]
    }
    override init(layer: Any) {
        textFont = (layer as? TextLayer)?.textFont ?? .systemFont(ofSize: 11)
        color = (layer as? TextLayer)?.color ?? .white
        super.init(layer: layer)
    }
    required init?(coder: NSCoder) { fatalError() }

    /// Vertically centres the single line in the layer's frame.
    override func draw(in ctx: CGContext) {
        let lineHeight = textFont.ascender - textFont.descender
        let offset = max(0, (bounds.height - lineHeight) / 2)
        ctx.saveGState()
        ctx.translateBy(x: 0, y: -offset)
        super.draw(in: ctx)
        ctx.restoreGState()
    }
}

/// One reusable tile: layer-backed thumbnail, kind badge, name, two tag chips,
/// size and date. Thumbnails load off the main thread and are dropped if the
/// cell is reused for another file first.
final class LibraryTileItem: NSCollectionViewItem {
    static let identifier = NSUserInterfaceItemIdentifier("LibraryTile")
    static let captionHeight: CGFloat = 68

    var onClick: ((Int) -> Void)?
    var onFindSimilar: (() -> Void)?
    var onOpenPreview: (() -> Void)?

    private let thumbLayer = CALayer()
    private let ringLayer = CALayer()
    private let badge = TextLayer(size: 10.5, weight: .semibold, color: NSColor(hex: 0xE2E5E8), align: .center)
    private let name = TextLayer(size: 13, weight: .medium, color: NSColor(hex: 0xF0F1F2), truncation: .middle)
    private let chips = [TextLayer(size: 11.5, weight: .medium, color: Theme.nsText2, align: .center),
                         TextLayer(size: 11.5, weight: .medium, color: Theme.nsText2, align: .center)]
    private let size = TextLayer(size: 11, weight: .regular, color: Theme.nsText3)
    private let date = TextLayer(size: 11, weight: .regular, color: Theme.nsText3, align: .right)
    private let check = NSImageView()
    private let textGlyph = NSImageView()
    private var hasSmartName = false
    private var rowID: Int64?
    private var thumbTask: Task<Void, Never>?
    private var hovering = false { didSet { updateRing() } }
    private var isChosen = false
    private var checked = false
    private var tracking: NSTrackingArea?

    override func loadView() {
        let root = TileView()
        root.wantsLayer = true
        root.owner = self
        thumbLayer.cornerRadius = 8
        thumbLayer.cornerCurve = .continuous
        thumbLayer.masksToBounds = true
        thumbLayer.contentsGravity = .resizeAspectFill
        thumbLayer.backgroundColor = NSColor.white.withAlphaComponent(0.05).cgColor
        ringLayer.cornerRadius = 8
        ringLayer.cornerCurve = .continuous
        ringLayer.borderWidth = 1
        root.layer?.addSublayer(thumbLayer)
        root.layer?.addSublayer(ringLayer)
        badge.backgroundColor = NSColor.black.withAlphaComponent(0.55).cgColor
        badge.cornerRadius = 4
        for chip in chips {
            chip.backgroundColor = NSColor.white.withAlphaComponent(0.07).cgColor
            chip.cornerRadius = 4
        }
        check.imageScaling = .scaleProportionallyUpOrDown
        textGlyph.image = NSImage(systemSymbolName: "text.viewfinder", accessibilityDescription: nil)
        textGlyph.contentTintColor = NSColor.white.withAlphaComponent(0.85)
        textGlyph.wantsLayer = true
        textGlyph.layer?.backgroundColor = NSColor.black.withAlphaComponent(0.55).cgColor
        textGlyph.layer?.cornerRadius = 4
        textGlyph.setAccessibilityElement(false)
        for layer in [badge, name, size, date] + chips { root.layer?.addSublayer(layer) }
        root.addSubview(check)
        root.addSubview(textGlyph)
        view = root
    }

    override func viewDidLayout() {
        super.viewDidLayout()
        let b = view.bounds
        let thumbHeight = (b.width * 3 / 4).rounded()
        CATransaction.begin(); CATransaction.setDisableActions(true)
        let thumbFrame = CGRect(x: 0, y: b.height - thumbHeight, width: b.width, height: thumbHeight)
        thumbLayer.frame = thumbFrame
        ringLayer.frame = (isChosen || checked) ? thumbFrame.insetBy(dx: -4, dy: -4) : thumbFrame
        CATransaction.commit()
        CATransaction.begin(); CATransaction.setDisableActions(true)
        badge.frame = CGRect(x: 6, y: b.height - 6 - 17, width: badge.textWidth + 12, height: 17)
        check.frame = CGRect(x: b.width - 26, y: b.height - 26, width: 18, height: 18)
        textGlyph.frame = CGRect(x: b.width - 6 - 20, y: b.height - thumbHeight + 6, width: 20, height: 18)
        var y = b.height - thumbHeight - 6 - 17
        name.frame = CGRect(x: 0, y: y, width: b.width, height: 17)
        y -= 4 + 18
        var x: CGFloat = 0
        for chip in chips where !chip.isHidden {
            let w = min(chip.textWidth + 14, (b.width - 4) / 2)
            chip.frame = CGRect(x: x, y: y, width: w, height: 18)
            x += w + 4
        }
        y -= 4 + 15
        size.frame = CGRect(x: 0, y: y, width: b.width / 2, height: 15)
        date.frame = CGRect(x: b.width / 2, y: y, width: b.width / 2, height: 15)
        CATransaction.commit()
    }

    func configure(row: FileRow, tags: [String], selected: Bool, selectMode: Bool, checked: Bool, reloadImage: Bool) {
        let changedFile = rowID != row.id
        rowID = row.id
        self.isChosen = selected
        self.checked = checked
        badge.text = row.kind.capitalized
        let proposed = row.vlmProposedName?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        hasSmartName = !proposed.isEmpty
        name.text = hasSmartName ? (row.extension.isEmpty ? proposed : "\(proposed).\(row.extension)") : row.url.lastPathComponent
        textGlyph.isHidden = !row.hasText
        for (index, chip) in chips.enumerated() {
            chip.isHidden = index >= tags.count
            if index < tags.count { chip.text = tags[index] }
        }
        size.text = ByteCountFormatter.string(fromByteCount: row.sizeBytes, countStyle: .file)
        date.text = row.displayDate?.formatted(date: .numeric, time: .omitted) ?? ""
        check.isHidden = !selectMode
        check.image = NSImage(systemSymbolName: checked ? "checkmark.circle.fill" : "circle", accessibilityDescription: nil)
        check.contentTintColor = checked ? NSColor(hex: 0xFFCC00) : NSColor.white.withAlphaComponent(0.7)
        updateRing()
        view.needsLayout = true
        view.setAccessibilityElement(true)
        view.setAccessibilityRole(.button)
        view.setAccessibilityLabel([row.url.lastPathComponent, hasSmartName ? "smart name \(name.text)" : nil, row.kind.capitalized,
                                    row.displayDate?.formatted(date: .abbreviated, time: .omitted),
                                    row.hasFaces ? "contains faces" : nil, row.hasText ? "contains text" : nil]
            .compactMap { $0 }.joined(separator: ", "))
        applyColors()
        view.setAccessibilitySelected(selected || checked)
        (view as? TileView)?.customActions = [NSAccessibilityCustomAction(name: "Open preview") { [weak self] in self?.onOpenPreview?(); return true }]
        if changedFile || reloadImage { loadThumbnail(for: row, keepCurrent: !changedFile) }
    }

    private func loadThumbnail(for row: FileRow, keepCurrent: Bool) {
        thumbTask?.cancel()
        if !keepCurrent { thumbLayer.contents = nil }
        let id = row.id
        thumbTask = Task { @MainActor [weak self] in
            let image = await ThumbnailService.shared.thumbnail(for: row.url, size: 224)
            guard let self, !Task.isCancelled, self.rowID == id else { return }
            CATransaction.begin(); CATransaction.setDisableActions(true)
            self.thumbLayer.contents = image?.cgImage(forProposedRect: nil, context: nil, hints: nil)
            CATransaction.commit()
        }
    }

    override func prepareForReuse() {
        super.prepareForReuse()
        thumbTask?.cancel()
        thumbTask = nil
        rowID = nil
        thumbLayer.contents = nil
        hovering = false
    }

    private func updateRing() {
        CATransaction.begin(); CATransaction.setDisableActions(true)
        if isChosen || checked {
            ringLayer.borderWidth = 2
            ringLayer.borderColor = NSColor(hex: 0xFFCC00).cgColor
            ringLayer.cornerRadius = 11
        } else {
            ringLayer.borderWidth = 1
            view.effectiveAppearance.performAsCurrentDrawingAppearance {
                ringLayer.borderColor = (hovering ? Theme.nsHairlineHover : Theme.nsHairline).cgColor
            }
            ringLayer.cornerRadius = 8
        }
        CATransaction.commit()
        view.needsLayout = true
    }

    func setHover(_ on: Bool) { if hovering != on { hovering = on } }

    /// Layer colours are CGColors, so dynamic tokens (Increase Contrast) are
    /// resolved here and again whenever the appearance changes.
    fileprivate func applyColors() {
        CATransaction.begin(); CATransaction.setDisableActions(true)
        view.effectiveAppearance.performAsCurrentDrawingAppearance {
            for layer in [badge, size, date] + chips { layer.foregroundColor = layer.color.cgColor }
            name.foregroundColor = (hasSmartName ? NSColor(hex: 0xFFCC00) : name.color).cgColor
        }
        CATransaction.commit()
        updateRing()
    }

    fileprivate func applyBackingScale(_ scale: CGFloat) {
        CATransaction.begin(); CATransaction.setDisableActions(true)
        for layer in [badge, name, size, date] + chips where layer.contentsScale != scale {
            layer.contentsScale = scale
            layer.setNeedsDisplay()
        }
        CATransaction.commit()
    }

    fileprivate func contextMenu() -> NSMenu {
        let menu = NSMenu()
        menu.addItem(ClosureMenuItem(title: "Open Preview") { [weak self] in self?.onOpenPreview?() })
        menu.addItem(ClosureMenuItem(title: "Find Similar Photos") { [weak self] in self?.onFindSimilar?() })
        menu.addItem(.separator())
        menu.addItem(ClosureMenuItem(title: "Show in Finder") { [weak self] in self?.onRevealInFinder?() })
        return menu
    }

    var onRevealInFinder: (() -> Void)?

    static func label(size: CGFloat, weight: NSFont.Weight, color: NSColor) -> NSTextField {
        let field = NSTextField(labelWithString: "")
        field.font = brandFont(size: size, weight: weight)
        field.textColor = color
        field.lineBreakMode = .byTruncatingTail
        field.maximumNumberOfLines = 1
        field.alignment = .left
        return field
    }

    nonisolated static func brandFont(size: CGFloat, weight: NSFont.Weight) -> NSFont {
        let base = NSFont(name: "Archivo", size: size) ?? .systemFont(ofSize: size, weight: weight)
        let descriptor = base.fontDescriptor.addingAttributes([.traits: [NSFontDescriptor.TraitKey.weight: weight]])
        return NSFont(descriptor: descriptor, size: size) ?? base
    }

    /// Same chip text as FileTile: last segment of hierarchical labels,
    /// capitalised, de-duplicated, two at most.
    static func displayTags(_ raw: [String]) -> [String] {
        var seen = Set<String>()
        return Array(raw.map { tag -> String in
            if tag.contains(" ") { return tag }
            let last = (tag.split(separator: "_").last.map(String.init) ?? tag).replacingOccurrences(of: "-", with: " ")
            guard let first = last.first else { return last }
            return first.uppercased() + last.dropFirst()
        }.filter { seen.insert($0.lowercased()).inserted }.prefix(2))
    }
}

private final class TileView: NSView {
    weak var owner: LibraryTileItem?
    var customActions: [NSAccessibilityCustomAction] = []

    override var isFlipped: Bool { false }
    override func mouseDown(with event: NSEvent) {
        if let grid = owner?.collectionView { window?.makeFirstResponder(grid) }
        owner?.onClick?(event.clickCount)
    }
    override func menu(for event: NSEvent) -> NSMenu? { owner?.contextMenu() }
    override func accessibilityPerformPress() -> Bool { owner?.onClick?(1); return true }
    override func accessibilityCustomActions() -> [NSAccessibilityCustomAction]? { customActions }
    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        owner?.applyColors()
    }
    override func viewDidChangeBackingProperties() {
        super.viewDidChangeBackingProperties()
        owner?.applyBackingScale(window?.backingScaleFactor ?? 2)
    }
}

private final class ClosureMenuItem: NSMenuItem {
    private let handler: () -> Void
    init(title: String, handler: @escaping () -> Void) {
        self.handler = handler
        super.init(title: title, action: #selector(run), keyEquivalent: "")
        target = self
    }
    required init(coder: NSCoder) { fatalError() }
    @objc private func run() { handler() }
}
