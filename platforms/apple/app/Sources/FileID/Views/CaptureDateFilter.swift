import SwiftUI
import FileIDShared

/// Optional from/to capture days for a catalog search. Days are whole local days:
/// the window runs from the start of the first day to the end of the last.
struct CaptureWindow: Equatable, Sendable {
    var from: Date?
    var to: Date?

    var isActive: Bool { from != nil || to != nil }

    /// Unix seconds for `capturedFrom` / `capturedTo`; never negative and never backwards.
    func bounds(calendar: Calendar = .current) -> (from: Double?, to: Double?) {
        var first = from.map { calendar.startOfDay(for: $0) }
        var last = to.map { calendar.startOfDay(for: $0) }
        if let a = first, let b = last, a > b { (first, last) = (b, a) }
        let start = first.map { max(0, $0.timeIntervalSince1970) }
        let end = last.map { day -> Double in
            let next = calendar.date(byAdding: .day, value: 1, to: day) ?? day.addingTimeInterval(86_400)
            return max(0, next.timeIntervalSince1970 - 0.001)
        }
        return (start, end)
    }

    func summary(calendar: Calendar = .current, locale: Locale = .current) -> String {
        let style = Date.FormatStyle(date: .abbreviated, time: .omitted, locale: locale, calendar: calendar, timeZone: calendar.timeZone)
        switch (from, to) {
        case (nil, nil): return "Any date"
        case let (start?, nil): return "From \(start.formatted(style))"
        case let (nil, end?): return "Until \(end.formatted(style))"
        case let (start?, end?):
            if calendar.isDate(start, inSameDayAs: end) { return start.formatted(style) }
            let (a, b) = start <= end ? (start, end) : (end, start)
            return "\(a.formatted(style)) – \(b.formatted(style))"
        }
    }

    func searchRequest(id: String, query: String, calendar: Calendar = .current) -> CatalogRequest {
        let window = bounds(calendar: calendar)
        return CatalogRequest(requestID: id, action: "search", query: query, capturedFrom: window.from, capturedTo: window.to)
    }
}

/// "Any date" button that opens From / To day pickers, plus a clear button while a window is set.
struct CaptureDateFilter: View {
    @Binding var window: CaptureWindow
    @State private var open = false

    var body: some View {
        HStack(spacing: 4) {
            Button { open.toggle() } label: {
                Label(window.summary(), systemImage: "calendar").lineLimit(1)
            }
            .buttonStyle(window.isActive ? AnyButtonStyle(DSAccentOutlineButtonStyle(height: Theme.Metrics.hitTarget))
                                         : AnyButtonStyle(DSSecondaryButtonStyle(height: Theme.Metrics.hitTarget)))
            .help("Only show files taken between two dates")
            .accessibilityLabel("Capture date")
            .accessibilityValue(window.summary())
            .accessibilityHint("Limits results to files taken between two dates")
            .popover(isPresented: $open, arrowEdge: .bottom) { editor }
            if window.isActive {
                DSIconButton("Clear dates", systemImage: "xmark.circle.fill", tint: Theme.text3) { set(CaptureWindow()) }
                    .transition(.opacity)
            }
        }
        .animation(Theme.spring, value: window.isActive)
    }

    private var editor: some View {
        VStack(alignment: .leading, spacing: 10) {
            SectionLabel("Captured")
            dayRow("From", date: $window.from, range: Date.distantPast...(window.to ?? Date.distantFuture),
                   fallback: { min(Calendar.current.date(byAdding: .month, value: -1, to: Self.today) ?? Self.today, window.to ?? Self.today) })
            dayRow("To", date: $window.to, range: (window.from ?? Date.distantPast)...Date.distantFuture,
                   fallback: { max(Self.today, window.from ?? Self.today) })
            HStack(spacing: 8) {
                Button("Clear") { set(CaptureWindow()) }
                    .buttonStyle(.dsGhost)
                    .disabled(!window.isActive)
                Spacer(minLength: 8)
                Button("Done") { open = false }
                    .buttonStyle(DSPrimaryButtonStyle(height: Theme.Metrics.hitTarget))
                    .keyboardShortcut(.defaultAction)
            }
        }
        .padding(14)
        .frame(width: 280)
        .background(Color(hex: 0x161618), ignoresSafeAreaEdges: [])
        .preferredColorScheme(.dark)
    }

    private func dayRow(_ label: String, date: Binding<Date?>, range: ClosedRange<Date>, fallback: @escaping () -> Date) -> some View {
        HStack(spacing: 10) {
            Toggle(isOn: Binding(get: { date.wrappedValue != nil },
                                 set: { on in date.wrappedValue = on ? Calendar.current.startOfDay(for: date.wrappedValue ?? fallback()) : nil })) {
                Text(label).frame(width: 40, alignment: .leading)
            }
            .toggleStyle(DSCheckboxToggleStyle())
            .accessibilityLabel(label == "From" ? "Limit start date" : "Limit end date")
            DatePicker("\(label) date",
                       selection: Binding(get: { date.wrappedValue ?? fallback() },
                                          set: { date.wrappedValue = Calendar.current.startOfDay(for: $0) }),
                       in: range, displayedComponents: .date)
                .datePickerStyle(.field)
                .labelsHidden()
                .disabled(date.wrappedValue == nil)
                .accessibilityLabel("\(label) date")
            Spacer(minLength: 0)
        }
        .frame(minHeight: Theme.Metrics.hitTarget)
    }

    private func set(_ next: CaptureWindow) {
        withAnimation(Theme.spring) { window = next }
    }

    private static var today: Date { Calendar.current.startOfDay(for: Date()) }
}
