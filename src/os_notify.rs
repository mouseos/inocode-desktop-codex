//! OS notifications (Codex Desktop: when a turn is done or waits for an approval, and the
//! window is not the one in front). A Windows toast made by PowerShell, so that nothing has to be
//! added to the build; the text goes in environment variables, not into the script.

const SCRIPT: &str = r#"
Add-Type -TypeDefinition 'using System; using System.Runtime.InteropServices; public class InoFg { [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow(); [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint p); }'
$p = 0
[void][InoFg]::GetWindowThreadProcessId([InoFg]::GetForegroundWindow(), [ref]$p)
if ($p -eq [uint32]$env:INOCODE_N_PID) { exit }
[void][Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType = WindowsRuntime]
[void][Windows.Data.Xml.Dom.XmlDocument, Windows.Data.Xml.Dom.XmlDocument, ContentType = WindowsRuntime]
$x = New-Object Windows.Data.Xml.Dom.XmlDocument
$t = [System.Security.SecurityElement]::Escape($env:INOCODE_N_TITLE)
$b = [System.Security.SecurityElement]::Escape($env:INOCODE_N_BODY)
$x.LoadXml("<toast><visual><binding template='ToastGeneric'><text>$t</text><text>$b</text></binding></visual></toast>")
$id = '{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\WindowsPowerShell\v1.0\powershell.exe'
[Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier($id).Show([Windows.UI.Notifications.ToastNotification]::new($x))
"#;

/// Tell the user (only when this window is not in front).
pub(crate) fn notify(title: &str, body: &str) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let (title, body) = (title.to_string(), body.to_string());
        std::thread::spawn(move || {
            let _ = std::process::Command::new("powershell")
                .args(["-NoProfile", "-NonInteractive", "-WindowStyle", "Hidden", "-Command", SCRIPT])
                .env("INOCODE_N_TITLE", title)
                .env("INOCODE_N_BODY", body)
                .env("INOCODE_N_PID", std::process::id().to_string())
                .creation_flags(0x0800_0000)
                .status();
        });
    }
    #[cfg(not(windows))]
    let _ = (title, body);
}
