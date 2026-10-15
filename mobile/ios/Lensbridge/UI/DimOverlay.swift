import SwiftUI
import UIKit

/// "Screen off" substitute for iOS: black full-screen overlay with minimum
/// backlight. The camera keeps running because the app stays in the foreground.
/// On OLED iPhones (13 Pro) black pixels are switched off, so this saves most
/// of the display power; on LCD models the backlight still glows at its minimum.
struct DimOverlay: View {
    var onExit: () -> Void
    @State private var savedBrightness: CGFloat = 0.5
    @State private var hintVisible = true

    var body: some View {
        Color.black
            .ignoresSafeArea()
            .overlay(
                Text("Экран затемнён, камера работает\nДвойное касание — выйти")
                    .font(.footnote)
                    .multilineTextAlignment(.center)
                    .foregroundColor(Color.white.opacity(0.25))
                    .opacity(hintVisible ? 1 : 0)
            )
            .contentShape(Rectangle())
            .onTapGesture(count: 2) { onExit() }
            .onTapGesture(count: 1) { flashHint() }
            .onAppear {
                savedBrightness = UIScreen.main.brightness
                UIScreen.main.brightness = 0
                flashHint()
            }
            .onDisappear {
                UIScreen.main.brightness = savedBrightness
            }
    }

    private func flashHint() {
        hintVisible = true
        DispatchQueue.main.asyncAfter(deadline: .now() + 3) {
            withAnimation { hintVisible = false }
        }
    }
}
