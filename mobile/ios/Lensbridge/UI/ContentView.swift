import SwiftUI

struct ContentView: View {
    @EnvironmentObject var model: AppModel
    @Environment(\.scenePhase) private var scenePhase

    var body: some View {
        ZStack {
            NavigationView {
                List {
                    statusSection
                    if model.cameraDenied || model.micDenied { permissionsSection }
                    connectionSection
                    PairedSection(store: model.pairing, onScan: model.startScanning)
                    settingsSection
                    limitsSection
                    eventsSection
                }
                .navigationTitle("Lensbridge")
            }
            .navigationViewStyle(.stack)

            if model.dimmed {
                DimOverlay { model.setDimmed(false) }
                    .transition(.opacity)
            }
        }
        .statusBar(hidden: model.dimmed)
        .sheet(isPresented: $model.showScanner) {
            ScannerSheet(onCode: model.handleScanned, onCancel: { model.showScanner = false })
        }
        .onChange(of: scenePhase) { model.scenePhaseChanged($0) }
    }

    private var statusSection: some View {
        Section {
            HStack(spacing: 10) {
                Circle()
                    .fill(model.connectedPC != nil ? Color.green : Color.orange)
                    .frame(width: 10, height: 10)
                VStack(alignment: .leading, spacing: 2) {
                    Text(model.connectedPC.map { "Передача на «\($0)»" } ?? "Ожидание подключения")
                        .font(.headline)
                    Text(model.status).font(.caption).foregroundColor(.secondary)
                }
            }
            if model.isCapturing {
                CameraPreview(session: model.capture.session)
                    .frame(height: 220)
                    .listRowInsets(EdgeInsets())
                HStack {
                    Text("\(model.fps) к/с")
                    Spacer()
                    Text(String(format: "%.1f Мбит/с", Double(model.kbps) / 1000))
                    if model.httpViewers > 0 {
                        Spacer()
                        Text("браузер: \(model.httpViewers)")
                    }
                }
                .font(.caption.monospacedDigit())
                .foregroundColor(.secondary)
            }
            Button {
                withAnimation { model.setDimmed(true) }
            } label: {
                Label("Затемнить экран (экономия батареи)", systemImage: "moon.fill")
            }
        }
    }

    private var permissionsSection: some View {
        Section(header: Text("Нужны разрешения")) {
            if model.cameraDenied { Text("Нет доступа к камере.") }
            if model.micDenied { Text("Нет доступа к микрофону — звук передаваться не будет.") }
            Button("Открыть Настройки") {
                if let url = URL(string: UIApplication.openSettingsURLString) { UIApplication.shared.open(url) }
            }
        }
    }

    private var connectionSection: some View {
        Section(header: Text("Адрес для ПК"), footer: Text("ПК находит телефон автоматически (Bonjour) или по USB. Если не находит — введите IP вручную, порт 4748.")) {
            if model.addresses.isEmpty {
                Text("Нет Wi-Fi-сети").foregroundColor(.secondary)
            }
            ForEach(model.addresses, id: \.self) { a in
                Text(a).font(.body.monospaced())
            }
            Button("Обновить") { model.refreshAddresses() }
        }
    }

    private var settingsSection: some View {
        Section(header: Text("Настройки")) {
            Picker("Ориентация видео", selection: $model.orientationMode) {
                Text("Авто").tag("auto")
                Text("Альбомная").tag("landscape")
                Text("Портретная").tag("portrait")
            }
            Toggle(isOn: $model.httpEnabled) {
                VStack(alignment: .leading) {
                    Text("Просмотр в браузере")
                    Text("http://IP:4747/video — без шифрования, только локальная сеть")
                        .font(.caption).foregroundColor(.secondary)
                }
            }
            HStack {
                Text("Имя телефона")
                TextField("iPhone", text: $model.deviceName)
                    .multilineTextAlignment(.trailing)
            }
        }
    }

    private var limitsSection: some View {
        Section(header: Text("Ограничения iOS")) {
            Text("iOS не даёт камеру приложениям в фоне. Держите Lensbridge открытым; чтобы экономить батарею, используйте затемнение — камера продолжит работать. Блокировка экрана, сворачивание или входящий звонок прерывают передачу, после возврата она восстановится сама.")
                .font(.footnote)
                .foregroundColor(.secondary)
        }
    }

    private var eventsSection: some View {
        Section(header: Text("События")) {
            ForEach(Array(model.events.reversed().prefix(20).enumerated()), id: \.offset) { _, e in
                Text(e).font(.caption.monospaced())
            }
        }
    }
}

struct PairedSection: View {
    @ObservedObject var store: PairingStore
    var onScan: () -> Void

    var body: some View {
        Section(header: Text("Сопряжённые ПК"), footer: Text("На ПК: Lensbridge → «Показать QR для телефона».")) {
            ForEach(store.pcs) { pc in
                VStack(alignment: .leading) {
                    Text(pc.name)
                    Text("ключ \(pc.keyIdHex) · \(pc.added.formatted(date: .abbreviated, time: .shortened))")
                        .font(.caption).foregroundColor(.secondary)
                }
            }
            .onDelete { idx in idx.map { store.pcs[$0] }.forEach(store.remove) }
            Button {
                onScan()
            } label: {
                Label("Сопрячь с ПК (сканировать QR)", systemImage: "qrcode.viewfinder")
            }
        }
    }
}

struct ScannerSheet: View {
    var onCode: (String) -> Void
    var onCancel: () -> Void

    var body: some View {
        ZStack(alignment: .bottom) {
            QRScannerView(onCode: onCode).ignoresSafeArea()
            VStack(spacing: 12) {
                Text("Наведите камеру на QR-код в приложении Lensbridge на ПК")
                    .multilineTextAlignment(.center)
                    .padding(12)
                    .background(.ultraThinMaterial)
                    .cornerRadius(10)
                Button("Отмена", action: onCancel)
                    .buttonStyle(.borderedProminent)
            }
            .padding(.bottom, 40)
            .padding(.horizontal)
        }
    }
}
