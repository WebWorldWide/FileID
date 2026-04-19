import SwiftUI

struct LavaLampBackground: View {
    var body: some View {
        ZStack {
            Color(white: 0.08) // Deep industrial black/gray
            
            TimelineView(.animation(minimumInterval: 0.016, paused: false)) { context in
                let time = context.date.timeIntervalSinceReferenceDate
                
                Canvas { ctx, size in
                    let w = size.width
                    let h = size.height
                    
                    let cx1 = w * 0.5 + sin(time * 0.2) * w * 0.3
                    let cy1 = h * 0.5 + cos(time * 0.23) * h * 0.3
                    
                    let cx2 = w * 0.5 + cos(time * 0.15) * w * 0.4
                    let cy2 = h * 0.5 + sin(time * 0.18) * h * 0.4
                    
                    let cx3 = w * 0.5 + sin(time * 0.1) * w * 0.2
                    let cy3 = h * 0.5 + cos(time * 0.12) * h * 0.2
                    
                    ctx.addFilter(.blur(radius: 120))
                    
                    ctx.fill(Path(ellipseIn: CGRect(x: cx1 - 400, y: cy1 - 400, width: 800, height: 800)), with: .color(Color(red: 1.0, green: 0.8, blue: 0.0).opacity(0.4)))
                    ctx.fill(Path(ellipseIn: CGRect(x: cx2 - 300, y: cy2 - 300, width: 600, height: 600)), with: .color(Color(red: 1.0, green: 0.4, blue: 0.0).opacity(0.3)))
                    ctx.fill(Path(ellipseIn: CGRect(x: cx3 - 500, y: cy3 - 500, width: 1000, height: 1000)), with: .color(Color(white: 0.05)))
                }
            }
            
            // Frosted dark glass overlay
            Rectangle()
                .fill(.ultraThinMaterial)
                .environment(\.colorScheme, .dark) // Force dark styling
        }
        .ignoresSafeArea()
    }
}

// Aqua Brushed Metal view for Sidebars
struct BrushedMetalBackground: View {
    var body: some View {
        LinearGradient(gradient: Gradient(colors: [Color(white: 0.8), Color(white: 0.6)]), startPoint: .leading, endPoint: .trailing)
            .overlay(
                // Adding a slight noise or repeating lines for brushed feel
                GeometryReader { geo in
                    Path { path in
                        let spacing: CGFloat = 1.0
                        let num = Int(geo.size.height / spacing)
                        for i in 0..<num {
                            let y = CGFloat(i) * spacing
                            path.move(to: CGPoint(x: 0, y: y))
                            path.addLine(to: CGPoint(x: geo.size.width, y: y))
                        }
                    }
                    .stroke(Color.black.opacity(0.03), lineWidth: 0.5)
                }
            )
            .ignoresSafeArea()
    }
}


