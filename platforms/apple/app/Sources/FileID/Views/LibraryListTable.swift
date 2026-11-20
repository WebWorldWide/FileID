import SwiftUI
import AppKit
import FileIDShared

/// Library list on NSTableView: thumbnail and name, date, size and kind columns. Rows are reused as they
/// scroll, header clicks ask the caller to re-sort in the database, and selection, checks and paging behave
/// exactly as in the tile grid.
struct LibraryListTable: NSViewRepresentable {
    let rows: [FileRow]
    let tagsByFile: [Int64: [String]]
    let selectMode: Bool
    let checkedIDs: Set<Int64>
    let selectedID: Int64?
    let refreshToken: String
    var contentVersion: Int = 0
    var scrollToTopToken: Int = 0
    var textSize: TextSize = .standard
    var offline: OfflineRoots = .none
    var sort: LibrarySort = .standard
    var onSort: (LibrarySortKey) -> Void = { _ in }
    var onSelect: (FileRow) -> Void
    var onOpen: (FileRow) -> Void
    var onToggleCheck: (FileRow) -> Void
    var onFindSimilar: (FileRow) -> Void
    var onNearEnd: () -> Void
    var onClearSelection: () -> Void = {}
    var onRangeCheck: (FileRow) -> Void = { _ in }
    var onCommandCheck: (FileRow) -> Void = { _ in }
    var onAtTopChange: (Bool) -> Void = { _ in }

    static let gutter = Theme.Metrics.pageInsetX - 8
    private static let nameColumn = NSUserInterfaceItemIdentifier("name")
    private static let dateColumn = NSUserInterfaceItemIdentifier("date")
    private static let sizeColumn = NSUserInterfaceItemIdentifier("size")
    private static let kindColumn = NSUserInterfaceItemIdentifier("kind")
    private static let leadingGutter = NSUserInterfaceItemIdentifier("leadingGutter")
    private static let trailingGutter = NSUserInterfaceItemIdentifier("trailingGutter")

    func makeCoordinator() -> Coordinator { Coordinator() }

    func makeNSView(context: Context) -> NSScrollView {
        let table = ListTableView()
        table.style = .plain
        table.backgroundColor = .clear
        table.usesAlternatingRowBackgroundColors = false
        table.selectionHighlightStyle = .none
        table.allowsColumnReordering = false
        table.allowsColumnSelection = false
        table.allowsEmptySelection = true
        table.intercellSpacing = NSSize(width: 0, height: 0)
        table.gridStyleMask = []
        table.columnAutoresizingStyle = .uniformColumnAutoresizingStyle
        table.focusRingType = .none
        table.setAccessibilityLabel("Files")
        let header = ListHeaderView(frame: NSRect(x: 0, y: 0, width: 0, height: 30))
        table.headerView = header

        func add(_ id: NSUserInterfaceItemIdentifier, title: String, width: CGFloat, min: CGFloat, max: CGFloat? = nil, sortKey: String? = nil, resizing: Bool = false) {
            let column = NSTableColumn(identifier: id)
            column.title = title
            column.width = width
            column.minWidth = min
            column.maxWidth = max ?? width
            column.resizingMask = resizing ? .autoresizingMask : []
            if let sortKey { column.sortDescriptorPrototype = NSSortDescriptor(key: sortKey, ascending: true) }
            if id == Self.sizeColumn { column.headerCell.alignment = .right }
            column.headerCell.font = LibraryTileItem.brandFont(size: 11.5, weight: .semibold)
            table.addTableColumn(column)
        }
        add(Self.leadingGutter, title: "", width: Self.gutter, min: Self.gutter)
        add(Self.nameColumn, title: "Name", width: 360, min: 200, max: 4_000, sortKey: "name", resizing: true)
        add(Self.dateColumn, title: "Date taken", width: 130, min: 110, sortKey: "date")
        add(Self.sizeColumn, title: "Size", width: 96, min: 80, sortKey: "size")
        add(Self.kindColumn, title: "Kind", width: 110, min: 90, sortKey: "kind")
        add(Self.trailingGutter, title: "", width: Self.gutter, min: Self.gutter)
        table.dataSource = context.coordinator
        table.delegate = context.coordinator
        table.onKey = { [weak coordinator = context.coordinator] key in coordinator?.handle(key) ?? false }
        table.onClick = { [weak coordinator = context.coordinator] row, count in coordinator?.click(row: row, count: count) }
        table.onMenu = { [weak coordinator = context.coordinator] row in coordinator?.menu(for: row) }

        let scroll = NSScrollView()
        scroll.documentView = table
        scroll.hasVerticalScroller = true
        scroll.drawsBackground = false
        scroll.automaticallyAdjustsContentInsets = false
        scroll.contentView.drawsBackground = false
        scroll.contentView.postsBoundsChangedNotifications = true
        scroll.postsFrameChangedNotifications = true
        context.coordinator.table = table
        NotificationCenter.default.addObserver(context.coordinator, selector: #selector(Coordinator.resized(_:)),
                                               name: NSView.frameDidChangeNotification, object: scroll)
        NotificationCenter.default.addObserver(context.coordinator, selector: #selector(Coordinator.scrolled(_:)),
                                               name: NSView.boundsDidChangeNotification, object: scroll.contentView)
        context.coordinator.apply(self, force: true)
        return scroll
    }

    func updateNSView(_ scroll: NSScrollView, context: Context) {
        context.coordinator.apply(self, force: false)
    }

    @MainActor
    final class Coordinator: NSObject, NSTableViewDataSource, NSTableViewDelegate {
        weak var table: ListTableView?
        var model: LibraryListTable?
        private var ids: [Int64] = []
        private var lastToken = ""
        private var lastSelectMode = false
        private var lastChecked: Set<Int64> = []
        private var lastSelected: Int64?
        private var lastTextSize: TextSize = .standard
        private var lastOffline: OfflineRoots = .none
        private var lastRows: [FileRow] = []
        private var lastContentVersion = Int.min
        private var lastScrollToTopToken = 0
        private var lastSort: LibrarySort?
        private var atTop = true
        private var lastClickRowID: Int64?
        private var cursorID: Int64?
        private var applyingSort = false

        @objc func scrolled(_ note: Notification) {
            guard let clip = note.object as? NSClipView else { return }
            let nowAtTop = clip.bounds.minY <= 80
            guard nowAtTop != atTop else { return }
            atTop = nowAtTop
            model?.onAtTopChange(nowAtTop)
        }

        /// Kind then date drop out when the list is too narrow for their columns, so Name keeps room and nothing is clipped.
        @objc func resized(_ note: Notification) { fitColumns() }

        private func fitColumns() {
            guard let table, let width = table.enclosingScrollView?.contentView.bounds.width, width > 0 else { return }
            let kind = table.tableColumn(withIdentifier: LibraryListTable.kindColumn)
            let date = table.tableColumn(withIdentifier: LibraryListTable.dateColumn)
            let showKind = width >= 560, showDate = width >= 440
            guard kind?.isHidden == showKind || date?.isHidden == showDate else { return }
            kind?.isHidden = !showKind
            date?.isHidden = !showDate
            table.sizeToFit()
            table.headerView?.needsDisplay = true
        }

        func apply(_ list: LibraryListTable, force: Bool) {
            model = list
            guard let table else { return }
            let contentChanged = force || list.contentVersion != lastContentVersion
            let newIDs = contentChanged ? list.rows.map(\.id) : ids
            let stateChanged = list.refreshToken != lastToken || list.selectMode != lastSelectMode
                || list.checkedIDs != lastChecked || list.selectedID != lastSelected || list.textSize != lastTextSize || list.offline != lastOffline
            if !list.selectMode { cursorID = nil }
            if force || list.textSize != lastTextSize {
                table.rowHeight = ListRowMetrics(list.textSize).rowHeight
                if !force { table.reloadData() }
            }
            if lastSort != list.sort { showSort(list.sort) }
            if force { fitColumns() }
            if force || newIDs != ids {
                let appended = !ids.isEmpty && newIDs.count > ids.count && Array(newIDs.prefix(ids.count)) == ids
                let old = ids.count
                ids = newIDs
                if appended {
                    NSAnimationContext.runAnimationGroup { $0.duration = 0; table.insertRows(at: IndexSet(old..<newIDs.count), withAnimation: []) }
                    if stateChanged || contentChanged { reconfigureVisible(list, reloadImages: list.refreshToken != lastToken) }
                } else {
                    table.reloadData()
                }
            } else if stateChanged || contentChanged {
                reconfigureVisible(list, reloadImages: list.refreshToken != lastToken)
            }
            if let selected = list.selectedID, selected != lastSelected, let index = newIDs.firstIndex(of: selected) {
                table.scrollRowToVisible(index)
            }
            lastToken = list.refreshToken
            lastSelectMode = list.selectMode
            lastChecked = list.checkedIDs
            lastSelected = list.selectedID
            lastTextSize = list.textSize
            lastOffline = list.offline
            lastRows = list.rows
            lastContentVersion = list.contentVersion
            lastSort = list.sort
            if list.scrollToTopToken != lastScrollToTopToken {
                lastScrollToTopToken = list.scrollToTopToken
                if let scroll = table.enclosingScrollView {
                    scroll.contentView.scroll(to: .zero)
                    scroll.reflectScrolledClipView(scroll.contentView)
                }
            }
        }

        private func showSort(_ sort: LibrarySort) {
            guard let table else { return }
            applyingSort = true
            defer { applyingSort = false }
            let dateColumn = table.tableColumn(withIdentifier: LibraryListTable.dateColumn)
            dateColumn?.title = sort.key == .dateAdded ? "Date added" : "Date taken"
            table.headerView?.needsDisplay = true
            let key: String? = switch sort.key {
            case .name: "name"
            case .dateTaken, .dateAdded: "date"
            case .size: "size"
            case .kind: "kind"
            }
            table.sortDescriptors = key.map { [NSSortDescriptor(key: $0, ascending: sort.ascending)] } ?? []
        }

        func tableView(_ tableView: NSTableView, sortDescriptorsDidChange oldDescriptors: [NSSortDescriptor]) {
            guard !applyingSort, let model, let clicked = tableView.sortDescriptors.first?.key else { return }
            let key: LibrarySortKey? = switch clicked {
            case "name": .name
            case "size": .size
            case "kind": .kind
            case "date": model.sort.key == .dateAdded ? .dateAdded : .dateTaken
            default: nil
            }
            if let key { model.onSort(key) }
            showSort(model.sort)
        }

        private func reconfigureVisible(_ list: LibraryListTable, reloadImages: Bool) {
            guard let table else { return }
            let oldPaths = Dictionary(lastRows.map { ($0.id, $0.pathText) }, uniquingKeysWith: { a, _ in a })
            let visible = table.rows(in: table.visibleRect)
            for index in visible.location..<(visible.location + visible.length) where index < list.rows.count {
                guard let rowView = table.rowView(atRow: index, makeIfNecessary: false) as? ListRowView else { continue }
                let row = list.rows[index]
                configure(rowView, row: row, reloadImage: reloadImages || oldPaths[row.id] != row.pathText)
            }
        }

        func numberOfRows(in tableView: NSTableView) -> Int { ids.count }

        func tableView(_ tableView: NSTableView, rowViewForRow row: Int) -> NSTableRowView? {
            let rowView = (tableView.makeView(withIdentifier: ListRowView.identifier, owner: nil) as? ListRowView) ?? ListRowView()
            rowView.identifier = ListRowView.identifier
            if let model, row < model.rows.count { configure(rowView, row: model.rows[row], reloadImage: true) }
            if row >= ids.count - 24 { model?.onNearEnd() }
            return rowView
        }

        func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? { nil }

        func tableView(_ tableView: NSTableView, shouldSelectRow row: Int) -> Bool { false }

        private func configure(_ rowView: ListRowView, row: FileRow, reloadImage: Bool) {
            guard let model else { return }
            rowView.configure(
                row: row,
                tags: LibraryTileItem.displayTags(model.tagsByFile[row.id] ?? [], kind: row.kind),
                sort: model.sort,
                selected: model.selectMode ? cursorID == row.id : model.selectedID == row.id,
                selectMode: model.selectMode,
                checked: model.checkedIDs.contains(row.id),
                offline: model.offline.isOffline(row.pathText),
                textSize: model.textSize,
                reloadImage: reloadImage)
        }

        func click(row index: Int, count: Int) {
            guard let model, index >= 0, index < model.rows.count else { model?.onClearSelection(); return }
            let row = model.rows[index]
            let sameRow = lastClickRowID == row.id
            lastClickRowID = row.id
            let flags = NSEvent.modifierFlags
            if flags.contains(.shift) { model.onRangeCheck(row) }
            else if model.selectMode { model.onToggleCheck(row) }
            else if flags.contains(.command) { model.onCommandCheck(row) }
            else if count >= 2 && sameRow { model.onOpen(row) }
            else { model.onSelect(row) }
        }

        func menu(for index: Int) -> NSMenu? {
            guard let model, index >= 0, index < model.rows.count else { return nil }
            let row = model.rows[index]
            let menu = NSMenu()
            menu.addItem(ListMenuItem(title: "Open Preview") { [weak self] in self?.model?.onOpen(row) })
            if !row.failed, row.kind == "image" || row.kind == "video" {
                menu.addItem(ListMenuItem(title: "Find Similar Photos and Videos") { [weak self] in self?.model?.onFindSimilar(row) })
            }
            menu.addItem(.separator())
            menu.addItem(ListMenuItem(title: "Show in Finder") { NSWorkspace.shared.activateFileViewerSelecting([row.url]) })
            return menu
        }

        func handle(_ key: ListTableView.Key) -> Bool {
            guard let model, let table, !model.rows.isEmpty else { return false }
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
            let start = current ?? -1
            let next: Int = switch key {
            case .up: start - 1
            case .down: start + 1
            default: start
            }
            let target = min(max(next, 0), model.rows.count - 1)
            guard target != current else { return true }
            if model.selectMode { moveCursor(to: model.rows[target].id) } else { model.onSelect(model.rows[target]) }
            table.scrollRowToVisible(target)
            if target >= model.rows.count - 24 { model.onNearEnd() }
            return true
        }

        private func moveCursor(to id: Int64?) {
            cursorID = id
            if let model { reconfigureVisible(model, reloadImages: false) }
        }
    }
}

struct ListRowMetrics: Equatable {
    let rowHeight: CGFloat
    let thumb: CGFloat

    init(_ textSize: TextSize) {
        thumb = (32 * (textSize == .standard ? 1 : textSize.factor)).rounded()
        rowHeight = thumb + 14
    }
}

private final class ListMenuItem: NSMenuItem {
    private let handler: () -> Void
    init(title: String, handler: @escaping () -> Void) {
        self.handler = handler
        super.init(title: title, action: #selector(run), keyEquivalent: "")
        target = self
    }
    required init(coder: NSCoder) { fatalError() }
    @objc private func run() { handler() }
}

/// Column titles in the app's meta style with a gold arrow on the column the list is sorted by; no separators between columns.
final class ListHeaderView: NSTableHeaderView {
    override func draw(_ dirtyRect: NSRect) {
        guard let table = tableView else { return }
        effectiveAppearance.performAsCurrentDrawingAppearance {
            Theme.nsHairline.setFill()
            NSRect(x: 0, y: bounds.height - 1, width: bounds.width, height: 1).fill()
            let current = table.sortDescriptors.first
            for (index, column) in table.tableColumns.enumerated() where column.sortDescriptorPrototype != nil && !column.isHidden {
                let slot = headerRect(ofColumn: index)
                let cell = NSRect(x: slot.minX + (column.identifier.rawValue == "name" ? 0 : 4), y: slot.minY, width: slot.width - 4 - (column.headerCell.alignment == .right ? 4 : 0), height: slot.height)
                let active = current?.key == column.sortDescriptorPrototype?.key
                let font = LibraryTileItem.brandFont(size: 11.5, weight: .semibold)
                let attributes: [NSAttributedString.Key: Any] = [.font: font, .foregroundColor: active ? Theme.nsGold : Theme.nsText3]
                let title = NSAttributedString(string: column.title, attributes: attributes)
                let textSize = title.size()
                let arrow = active ? NSImage(systemSymbolName: current?.ascending == true ? "chevron.up" : "chevron.down", accessibilityDescription: nil)?
                    .withSymbolConfiguration(.init(pointSize: 9, weight: .bold)) : nil
                let arrowWidth: CGFloat = arrow == nil ? 0 : 15
                let right = column.headerCell.alignment == .right
                let textX = right ? cell.maxX - textSize.width : cell.minX
                let y = (bounds.height - textSize.height) / 2
                title.draw(at: NSPoint(x: textX, y: y))
                if let arrow {
                    let tinted = NSImage(size: arrow.size, flipped: false) { rect in
                        arrow.draw(in: rect)
                        Theme.nsGold.set()
                        rect.fill(using: .sourceAtop)
                        return true
                    }
                    let arrowX = right ? textX - arrowWidth + 3 : textX + textSize.width + 6
                    tinted.draw(in: NSRect(x: arrowX, y: (bounds.height - arrow.size.height) / 2, width: arrow.size.width, height: arrow.size.height))
                }
            }
        }
    }
}

final class ListTableView: NSTableView {
    enum Key { case up, down, open, clear }
    var onKey: ((Key) -> Bool)?
    var onClick: ((Int, Int) -> Void)?
    var onMenu: ((Int) -> NSMenu?)?
    override var acceptsFirstResponder: Bool { true }
    override var focusRingMaskBounds: NSRect { .zero }

    override func keyDown(with event: NSEvent) {
        let key: Key? = switch event.keyCode {
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
        onClick?(row(at: convert(event.locationInWindow, from: nil)), event.clickCount)
    }

    override func menu(for event: NSEvent) -> NSMenu? {
        onMenu?(row(at: convert(event.locationInWindow, from: nil)))
    }

    private var hoverArea: NSTrackingArea?
    private weak var hovered: ListRowView?

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        if let hoverArea { removeTrackingArea(hoverArea) }
        let area = NSTrackingArea(rect: .zero, options: [.mouseMoved, .mouseEnteredAndExited, .activeInKeyWindow, .inVisibleRect], owner: self)
        addTrackingArea(area)
        hoverArea = area
    }

    override func mouseMoved(with event: NSEvent) {
        let index = row(at: convert(event.locationInWindow, from: nil))
        let rowView = index >= 0 ? self.rowView(atRow: index, makeIfNecessary: false) as? ListRowView : nil
        guard rowView !== hovered else { return }
        hovered?.setHover(false)
        rowView?.setHover(true)
        hovered = rowView
    }

    override func mouseExited(with event: NSEvent) {
        hovered?.setHover(false)
        hovered = nil
    }
}

/// One reusable list row, drawn with Core Animation layers: no per-cell views, so scrolling tens of thousands of files stays cheap.
final class ListRowView: NSTableRowView {
    static let identifier = NSUserInterfaceItemIdentifier("LibraryListRow")

    private let thumbLayer = CALayer()
    private let checkView = NSImageView()
    private let kindGlyph = NSImageView()
    private let unreadableGlyph = NSImageView()
    private let name = TextLayer(size: 13, weight: .medium, color: Theme.nsText1, truncation: .middle)
    private let detail = TextLayer(size: 11.5, weight: .regular, color: Theme.nsText3, truncation: .middle)
    private let offlineBadge = TextLayer(size: 10.5, weight: .semibold, color: Theme.nsWarning, align: .center)
    private let date = TextLayer(size: 12.5, weight: .regular, color: Theme.nsText2)
    private let size = TextLayer(size: 12.5, weight: .regular, color: Theme.nsText2, align: .right)
    private let kind = TextLayer(size: 12.5, weight: .regular, color: Theme.nsText2)
    private let separator = CALayer()
    private var rowID: Int64?
    private var thumbTask: Task<Void, Never>?
    private var chosen = false
    private var checked = false
    private var hovering = false { didSet { needsDisplay = true } }
    private var textSize: TextSize = .standard
    private var showsCheck = false

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        wantsLayer = true
        thumbLayer.cornerRadius = 6
        thumbLayer.cornerCurve = .continuous
        thumbLayer.masksToBounds = true
        thumbLayer.contentsGravity = .resizeAspectFill
        thumbLayer.backgroundColor = NSColor.white.withAlphaComponent(0.05).cgColor
        offlineBadge.backgroundColor = NSColor.black.withAlphaComponent(0.55).cgColor
        offlineBadge.borderColor = Theme.nsWarning.withAlphaComponent(0.6).cgColor
        offlineBadge.borderWidth = 1
        offlineBadge.cornerRadius = 4
        offlineBadge.text = "OFFLINE"
        offlineBadge.isHidden = true
        checkView.imageScaling = .scaleProportionallyUpOrDown
        kindGlyph.contentTintColor = Theme.nsText3
        kindGlyph.isHidden = true
        kindGlyph.setAccessibilityElement(false)
        unreadableGlyph.image = NSImage(systemSymbolName: "exclamationmark.triangle", accessibilityDescription: nil)
        unreadableGlyph.contentTintColor = Theme.nsText3
        unreadableGlyph.setAccessibilityElement(false)
        checkView.setAccessibilityElement(false)
        separator.backgroundColor = NSColor.white.withAlphaComponent(0.06).cgColor
        for layer in [thumbLayer, name, detail, offlineBadge, date, size, kind, separator] { self.layer?.addSublayer(layer) }
        addSubview(kindGlyph)
        addSubview(unreadableGlyph)
        addSubview(checkView)
        setAccessibilityRole(.button)
    }

    required init?(coder: NSCoder) { fatalError() }

    override var isFlipped: Bool { false }
    override var isEmphasized: Bool { get { false } set {} }

    private var columnFrames: (name: CGRect, date: CGRect, size: CGRect, kind: CGRect) {
        guard let table = superview as? ListTableView else { return (.zero, .zero, .zero, .zero) }
        func frame(_ id: String) -> CGRect {
            let index = table.column(withIdentifier: NSUserInterfaceItemIdentifier(id))
            guard index >= 0 else { return .zero }
            return table.rect(ofColumn: index)
        }
        return (frame("name"), frame("date"), frame("size"), frame("kind"))
    }

    override func layout() {
        super.layout()
        let metrics = ListRowMetrics(textSize)
        let columns = columnFrames
        let b = bounds
        CATransaction.begin(); CATransaction.setDisableActions(true)
        let checkWidth: CGFloat = showsCheck ? 26 : 0
        let thumbFrame = CGRect(x: columns.name.minX + checkWidth, y: (b.height - metrics.thumb) / 2, width: metrics.thumb, height: metrics.thumb)
        thumbLayer.frame = thumbFrame
        kindGlyph.frame = thumbFrame.insetBy(dx: metrics.thumb * 0.2, dy: metrics.thumb * 0.2)
        unreadableGlyph.frame = kindGlyph.frame
        checkView.frame = CGRect(x: columns.name.minX, y: (b.height - 18) / 2, width: 18, height: 18)
        let textX = thumbFrame.maxX + 12
        let textWidth = max(0, columns.name.maxX - 12 - textX)
        let lineHeight = ceil(textSize.scaled(13) * 1.35)
        let detailHeight = ceil(textSize.scaled(11.5) * 1.35)
        let hasDetail = !detail.text.isEmpty
        let blockHeight = hasDetail ? lineHeight + 2 + detailHeight : lineHeight
        let blockBottom = (b.height - blockHeight) / 2
        let nameY = blockBottom + (hasDetail ? detailHeight + 2 : 0)
        var nameWidth = textWidth
        if !offlineBadge.isHidden {
            let badgeWidth = offlineBadge.textWidth + 12
            nameWidth = max(0, textWidth - badgeWidth - 6)
            offlineBadge.frame = CGRect(x: textX + min(name.textWidth, nameWidth) + 6, y: nameY + (lineHeight - 17) / 2, width: badgeWidth, height: 17)
        }
        name.frame = CGRect(x: textX, y: nameY, width: nameWidth, height: lineHeight)
        detail.frame = CGRect(x: textX, y: blockBottom, width: textWidth, height: detailHeight)
        let metaHeight = ceil(textSize.scaled(12.5) * 1.35)
        let metaY = (b.height - metaHeight) / 2
        date.frame = CGRect(x: columns.date.minX + 4, y: metaY, width: max(0, columns.date.width - 8), height: metaHeight)
        size.frame = CGRect(x: columns.size.minX, y: metaY, width: max(0, columns.size.width - 8), height: metaHeight)
        kind.frame = CGRect(x: columns.kind.minX + 4, y: metaY, width: max(0, columns.kind.width - 8), height: metaHeight)
        let inset = LibraryListTable.gutter
        separator.frame = CGRect(x: inset, y: 0, width: max(0, b.width - 2 * inset), height: 1)
        CATransaction.commit()
    }

    func configure(row: FileRow, tags: [String], sort: LibrarySort, selected: Bool, selectMode: Bool, checked: Bool, offline: Bool,
                   textSize: TextSize, reloadImage: Bool) {
        let changedFile = rowID != row.id
        if textSize != self.textSize {
            self.textSize = textSize
            CATransaction.begin(); CATransaction.setDisableActions(true)
            for layer in [name, detail, offlineBadge, date, size, kind] { layer.apply(textSize) }
            CATransaction.commit()
        }
        rowID = row.id
        chosen = selected
        self.checked = checked
        showsCheck = selectMode
        let fileName = row.url.lastPathComponent
        name.text = fileName
        let described = (row.vlmProposedName ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
        detail.text = row.failed ? (row.errorMessage ?? "Couldn't be read") : tags.joined(separator: " · ")
        offlineBadge.isHidden = !offline
        let shownDate = sort.key == .dateAdded ? row.scannedAt : row.displayDate
        date.text = shownDate?.formatted(date: .abbreviated, time: .omitted) ?? "—"
        size.text = row.sizeBytes == 0 ? "0 bytes" : ByteCountFormatter.string(fromByteCount: row.sizeBytes, countStyle: .file)
        kind.text = row.failed ? "Unreadable" : FileKindLabel.title(row.kind)
        checkView.isHidden = !selectMode
        checkView.image = NSImage(systemSymbolName: checked ? "checkmark.circle.fill" : "circle", accessibilityDescription: nil)
        checkView.contentTintColor = checked ? Theme.nsGold : NSColor.white.withAlphaComponent(0.7)
        unreadableGlyph.isHidden = !row.failed
        setAccessibilityLabel([fileName, described.isEmpty ? nil : "suggested name \(described)", row.failed ? "couldn't be read" : FileKindLabel.title(row.kind),
                               offline ? "offline" : nil, shownDate?.formatted(date: .abbreviated, time: .omitted),
                               ByteCountFormatter.string(fromByteCount: row.sizeBytes, countStyle: .file)]
            .compactMap { $0 }.joined(separator: ", "))
        setAccessibilitySelected(selected || checked)
        applyColors()
        needsDisplay = true
        needsLayout = true
        if row.failed { thumbTask?.cancel(); thumbLayer.contents = nil; kindGlyph.isHidden = true }
        else if changedFile || reloadImage { loadThumbnail(for: row, keepCurrent: !changedFile) }
    }

    private func loadThumbnail(for row: FileRow, keepCurrent: Bool) {
        thumbTask?.cancel()
        if !keepCurrent { thumbLayer.contents = nil; thumbLayer.contentsRect = CGRect(x: 0, y: 0, width: 1, height: 1); kindGlyph.isHidden = true }
        let id = row.id
        thumbTask = Task { @MainActor [weak self] in
            let image = await ThumbnailService.shared.thumbnail(for: row.url, size: 224)
            guard let self, !Task.isCancelled, self.rowID == id else { return }
            let cgImage = image?.cgImage(forProposedRect: nil, context: nil, hints: nil)
            let isPage = row.kind == "doc" || row.kind == "pdf"
            let crop = isPage ? cgImage.flatMap { PageThumbnail.topCrop(width: $0.width, height: $0.height) } : nil
            let blank = cgImage == nil || (isPage && cgImage.map { PageThumbnail.isBlank($0, crop: crop) } == true)
            CATransaction.begin(); CATransaction.setDisableActions(true)
            self.thumbLayer.contentsRect = blank ? CGRect(x: 0, y: 0, width: 1, height: 1) : (crop ?? CGRect(x: 0, y: 0, width: 1, height: 1))
            self.thumbLayer.contents = blank ? nil : cgImage
            self.kindGlyph.image = blank ? NSImage(systemSymbolName: FileKind.symbolName(forKind: row.kind), accessibilityDescription: nil)?
                .withSymbolConfiguration(.init(pointSize: 16, weight: .light)) : nil
            self.kindGlyph.isHidden = !blank
            CATransaction.commit()
        }
    }

    override func prepareForReuse() {
        super.prepareForReuse()
        thumbTask?.cancel()
        thumbTask = nil
        rowID = nil
        thumbLayer.contents = nil
        kindGlyph.isHidden = true
        hovering = false
    }

    func setHover(_ on: Bool) { if hovering != on { hovering = on } }

    private func applyColors() {
        CATransaction.begin(); CATransaction.setDisableActions(true)
        effectiveAppearance.performAsCurrentDrawingAppearance {
            for layer in [name, detail, offlineBadge, date, size, kind] { layer.foregroundColor = layer.color.cgColor }
            separator.backgroundColor = Theme.nsHairline.cgColor
        }
        CATransaction.commit()
    }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        applyColors()
    }

    override func viewDidChangeBackingProperties() {
        super.viewDidChangeBackingProperties()
        let scale = window?.backingScaleFactor ?? 2
        CATransaction.begin(); CATransaction.setDisableActions(true)
        for layer in [name, detail, offlineBadge, date, size, kind] where layer.contentsScale != scale {
            layer.contentsScale = scale
            layer.setNeedsDisplay()
        }
        CATransaction.commit()
    }

    override func menu(for event: NSEvent) -> NSMenu? { (superview as? ListTableView)?.menu(for: event) }

    override func drawBackground(in dirtyRect: NSRect) {}
    override func drawSelection(in dirtyRect: NSRect) {}

    override func draw(_ dirtyRect: NSRect) {
        guard chosen || checked || hovering else { return }
        let inset = LibraryListTable.gutter - 6
        let rect = bounds.insetBy(dx: inset, dy: 2)
        let path = NSBezierPath(roundedRect: rect, xRadius: 8, yRadius: 8)
        effectiveAppearance.performAsCurrentDrawingAppearance {
            if chosen || checked {
                Theme.nsGold.withAlphaComponent(0.1).setFill()
                path.fill()
                Theme.nsGold.setStroke()
                path.lineWidth = 1.5
                path.stroke()
            } else {
                NSColor.white.withAlphaComponent(0.05).setFill()
                path.fill()
            }
        }
    }
}
