// The user's settings, the same in every shell (viewer-core keeps them): the Mac's
// Settings window, and a sheet on iPhone and iPad.

import SwiftUI

struct SettingsView: View {
    @EnvironmentObject var shell: Shell

    var body: some View {
        Form {
            Picker("Appearance", selection: $shell.settings.theme) {
                Text("As the system").tag(ThemeChoice.system)
                Text("Light").tag(ThemeChoice.light)
                Text("Dark").tag(ThemeChoice.dark)
            }
            Toggle("Reopen the last file at launch", isOn: $shell.settings.reopenLast)
            Section {
                Button("Clear Recent Files", role: .destructive) { shell.clearRecents() }
                    .disabled(shell.recents.isEmpty)
            }
        }
        #if os(macOS)
        .formStyle(.grouped)
        .frame(width: 420)
        .fixedSize()
        #endif
    }
}
