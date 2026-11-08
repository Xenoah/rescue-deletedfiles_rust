# Diagnostic only: compare the same Microsoft IME and key sequence against a
# standard WPF TextBox on the same disposable host, after the product test.
param([Parameter(Mandatory=$true)][string]$Config)
. (Join-Path $PSScriptRoot 'gui_windows.ps1') -Config $Config -DefinitionsOnly
Add-Type -ReferencedAssemblies PresentationFramework,PresentationCore,WindowsBase,System.Xaml -TypeDefinition @'
using System;
using System.Threading;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Interop;
using System.Windows.Automation;
public static class RescueImeBaseline {
    static Window window;
    static ManualResetEvent ready=new ManualResetEvent(false);
    public static IntPtr Handle;
    public static string Error;
    public static void Start() {
        var thread=new Thread(() => {
            try {
                var edit=new TextBox {Margin=new Thickness(20),FontSize=24};
                AutomationProperties.SetName(edit,"Search results");
                window=new Window {Title="Rescue owned WPF IME baseline",Width=800,Height=240,Content=edit};
                window.Show();Handle=new WindowInteropHelper(window).Handle;edit.Focus();ready.Set();
                System.Windows.Threading.Dispatcher.Run();
            } catch(Exception ex) {Error=ex.ToString();ready.Set();}
        });
        thread.SetApartmentState(ApartmentState.STA);thread.IsBackground=true;thread.Start();
        if(!ready.WaitOne(10000) || Error!=null)throw new Exception("Owned WPF IME baseline failed: "+Error);
    }
    public static void Stop() {
        if(window!=null)window.Dispatcher.Invoke(new Action(() => {window.Close();System.Windows.Threading.Dispatcher.ExitAllFrames();}));
    }
}
'@
$record=@{scope='Diagnostic standard WPF TextBox, same token and IME setup; never release proof';status='failed';cycles=@()}
function Observe-Baseline([string]$Stage,[string]$Expected) {
    $timer=[System.Diagnostics.Stopwatch]::StartNew()
    do {
        Start-Sleep -Milliseconds 100
        $value=(Await-Control 'Search results' 'Edit').GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value
    } while ($value -cne $Expected -and $timer.Elapsed.TotalSeconds -lt 10)
    $sample=@{stage=$Stage;expected=$Expected;value=$value;elapsed_ms=[int]$timer.Elapsed.TotalMilliseconds}
    $script:current.observations+=,$sample
    $record|ConvertTo-Json -Depth 10|Set-Content -LiteralPath $cfg.result -Encoding UTF8
    Save-OwnedScreenshot ("windows-wpf-{0}-{1}.png" -f $script:cycle,$Stage)
    return $value -ceq $Expected
}
try {
    [RescueImeBaseline]::Start()
    $script:owned=[pscustomobject]@{Id=$PID;MainWindowHandle=[RescueImeBaseline]::Handle;HasExited=$false}
    $script:owned|Add-Member -MemberType ScriptMethod -Name Refresh -Value {}
    $service='{03B5835F-F03C-411B-9CE2-AA23E1171E36}';$profile='{A76C93D9-5523-4E90-AAFA-4DB112F9AC76}'
    $tips=@(foreach ($language in (Get-WinUserLanguageList)) {if ($language.LanguageTag -match '^ja($|-)') {$language.InputMethodTips}})
    if ($tips -notcontains "0411:$service$profile") {throw 'Prepare the Japanese user language profile before the WPF comparison'}
    Focus-Owned (Await-Control 'Search results' 'Edit')
    $record.description=[RescueOwnedWindow]::EnableJapaneseIme($script:owned.MainWindowHandle,$service,$profile)
    Start-Sleep -Milliseconds 500
    $cancel=-join [char[]](0x3068,0x308a,0x3051,0x3057)
    $kana=-join [char[]](0x3052,0x3093,0x3076,0x3093)
    for ($script:cycle=1;$script:cycle -le 1;$script:cycle++) {
        $script:current=@{cycle=$script:cycle;observations=@()};$record.cycles+=,$script:current
        Focus-Owned (Await-Control 'Search results' 'Edit')
        if ($script:cycle -gt 1) {
            [RescueOwnedWindow]::Key(0x11,$false,$false);[RescueOwnedWindow]::Tap(0x41);[RescueOwnedWindow]::Key(0x11,$true,$false);[RescueOwnedWindow]::Tap(8)
        }
        [RescueOwnedWindow]::RomanKeys('torikeshi')
        $null=Observe-Baseline 'cancel-preedit' $cancel
        [RescueOwnedWindow]::Tap(27);Start-Sleep -Milliseconds 400
        [RescueOwnedWindow]::Tap(27)
        if (!(Observe-Baseline 'cancelled' '')) {throw 'Native WPF cancellation also failed'}
        Focus-Owned (Await-Control 'Search results' 'Edit')
        [RescueOwnedWindow]::RomanKeys('genbunn')
        $null=Observe-Baseline 'kana' $kana
        [RescueOwnedWindow]::Tap(0x20)
        $null=Observe-Baseline 'converted' $cfg.query
        [RescueOwnedWindow]::Tap(13)
        if (!(Observe-Baseline 'committed' $cfg.query)) {throw 'Native WPF conversion also failed'}
    }
    $record.status='passed'
} catch {$record.error=$_.Exception.ToString();throw}
finally {
    $record|ConvertTo-Json -Depth 10|Set-Content -LiteralPath $cfg.result -Encoding UTF8
    try {
        if ($null -ne $script:owned) {
            [RescueOwnedWindow]::RestoreJapaneseIme($script:owned.MainWindowHandle)
        }
    } finally {[RescueImeBaseline]::Stop()}
}
