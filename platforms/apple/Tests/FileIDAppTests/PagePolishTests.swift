import Testing
@testable import FileID

@Suite("Page polish rules")
struct PagePolishTests {
    @Test func tileTagsDropTheOneThatRepeatsTheKindBadge() {
        #expect(TagLabel.tileChips(["Video", "Beach", "Sunset"], kind: "video") == ["Beach", "Sunset"])
        #expect(TagLabel.tileChips(["doc", "Invoice"], kind: "doc") == ["Invoice"])
        #expect(TagLabel.tileChips(["PDF"], kind: "pdf").isEmpty)
        #expect(TagLabel.tileChips(["Pumpkin", "Hay", "Man"], kind: "image") == ["Pumpkin", "Hay"])
    }

    @Test func relationChipOnlyMarksExceptions() {
        let same: [String?] = ["repeatedAttempts", "repeatedAttempts", "repeatedAttempts"]
        #expect(!TakeProposals.showsRelationChip("repeatedAttempts", among: same))
        let mixed: [String?] = ["repeatedAttempts", "repeatedAttempts", "uncertain"]
        #expect(!TakeProposals.showsRelationChip("repeatedAttempts", among: mixed))
        #expect(TakeProposals.showsRelationChip("uncertain", among: mixed))
        #expect(TakeProposals.showsRelationChip("alternateViews", among: ["alternateViews"]))
        #expect(TakeProposals.showsRelationChip("alternateViews", among: ["alternateViews", "repeatedAttempts"]))
    }

    @Test func relationChipSetIsComputedFromTheWholeSection() {
        #expect(TakeProposals.relationsShowingChip(among: ["repeatedAttempts", "repeatedAttempts", "uncertain"]) == ["uncertain"])
        #expect(TakeProposals.relationsShowingChip(among: ["alternateViews", "repeatedAttempts"]) == ["alternateViews", "repeatedAttempts"])
        #expect(TakeProposals.relationsShowingChip(among: ["repeatedAttempts"]) == ["repeatedAttempts"])
    }

    @Test func menuLabelDimsWhenDisabled() {
        #expect(DSMenuLabel.opacity(isEnabled: true) == 1)
        #expect(DSMenuLabel.opacity(isEnabled: false) == Theme.disabledOpacity)
    }

    @Test func startFailureCopyNeverCallsTheHelperAnEngine() {
        let causes: [EngineStartFailure.Cause] = [.missing, .signatureMismatch, .couldNotLaunch, .exitsAtStart, .keepsStopping]
        for cause in causes {
            #expect(!EngineStartFailure(cause).summary.lowercased().contains("engine"), "\(cause)")
            #expect(!EngineUnavailableCard.reinstallHint(for: EngineStartFailure(cause)).lowercased().contains("engine"), "\(cause)")
        }
        #expect(!EngineUnavailableCard.title.lowercased().contains("engine"))
    }

    @Test func disabledControlsKeepLegibleText() {
        #expect(Theme.disabledOpacity >= 0.55)
    }
}

@Suite struct ShimmerRadiusTests {
    @Test func theShimmerClipsToTheRadiusItIsGiven() throws {
        let source = try appSource("Views/MotionPrimitives.swift")
        let shimmer = try #require(source.range(of: "struct ShimmerView"))
        #expect(source[shimmer.upperBound...].contains(".clipShape(RoundedRectangle(cornerRadius: cornerRadius, style: .continuous))"))
    }
}

@Suite struct TakeListLimitTests {
    @Test func aFullListSaysItMayHideMoreAndAShortOneStaysQuiet() {
        #expect(TakeProposals.listLimitNote(shown: 100)?.contains("first 100") == true)
        #expect(TakeProposals.listLimitNote(shown: 99) == nil)
        #expect(TakeProposals.listLimit == 100)
    }

    @Test func theOrderTextMatchesTheBlendedRanking() {
        #expect(!TakeProposals.possibleGroupsOrder.contains("Biggest and newest"))
        #expect(TakeProposals.possibleGroupsOrder.contains("Bigger groups and more recent"))
    }
}

@Suite struct LargerTextLayoutTests {
    @Test func chatChipsAndComposerWrapInsteadOfBreakingWords() throws {
        let chat = try appSource("Views/ChatWorkbench.swift")
        #expect(chat.contains("FlowLayout(spacing: 6) {\n                    ForEach(isPage ? Self.lookingIn"))
        #expect(chat.contains("FlowLayout(spacing: 10) {\n                Toggle(isPage ? \"Summarize with local AI\""))
        #expect(chat.contains("Label(\"AI model\", systemImage: \"cpu\").font(Theme.Font.body(12, .semibold)).lineLimit(1).fixedSize()"))
    }

    @Test func panelHeadersAndOptionRowsFallBackToStackedLayouts() throws {
        let design = try appSource("Theme/DesignSystem.swift")
        let header = try #require(design.range(of: "struct PanelHeader"))
        #expect(design[header.lowerBound...].prefix(900).contains("ViewThatFits(in: .horizontal)"))
        let tools = try appSource("Views/ToolsWorkbench.swift")
        let row = try #require(tools.range(of: "private func optionRow"))
        #expect(tools[row.lowerBound...].prefix(900).contains("ViewThatFits(in: .horizontal)"))
        #expect(tools[row.lowerBound...].prefix(900).contains("VStack(alignment: .leading, spacing: 4)"))
    }

    @Test func bestTakesActionsAndPaneHeadersDoNotTruncate() throws {
        let review = try appSource("Views/TakeReview.swift")
        #expect(review.contains("FlowLayout(spacing: 8) {\n                Button(primaryTitle)"))
        #expect(review.contains("SectionLabel(\"\\(title) · \\(index) of \\(qualities.count)\", color: tone).fixedSize()"))
    }

    @Test func pageHeaderAndLibraryToolbarKeepTheDesignedSize() throws {
        let detail = try appSource("Views/Detail.swift")
        let header = try #require(detail.range(of: "private var pageHeader"))
        #expect(detail[header.lowerBound...].prefix(900).contains(".environment(\\.fixedTextSize, true)"))
        let library = try appSource("Views/LibraryView.swift")
        #expect(library.contains("TextField(\"Filter files\", text: $searchText)"))
        #expect(library.contains(".font(Theme.Font.body(13.5, fixed: true))\n                .focused($searchFocused)"))
        #expect(library.contains(".environment(\\.fixedTextSize, true)\n        .padding(.horizontal, Theme.Metrics.pageInsetX).padding(.top, Theme.Metrics.pageInsetY)"))
    }
}
