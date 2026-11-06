# Own-process UI Automation on a disposable native Windows runner. No host
# storage, external windows, or user files are selected.
param([Parameter(Mandatory=$true)][string]$Config)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
Add-Type -AssemblyName PresentationFramework, PresentationCore, WindowsBase, System.Xaml
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class RescueOwnedWindow {
    [DllImport("UIAutomationCore.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static extern bool UiaHasServerSideProvider(IntPtr hwnd);
    [DllImport("user32.dll", SetLastError=true)]
    public static extern IntPtr SendMessageTimeout(IntPtr hwnd, uint msg, UIntPtr wparam, IntPtr lparam, uint flags, uint timeout, out UIntPtr result);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)]
    public static extern IntPtr GetProp(IntPtr hwnd, string name);
    [DllImport("kernel32.dll")]
    public static extern uint WTSGetActiveConsoleSessionId();
    [DllImport("user32.dll")]
    public static extern IntPtr GetProcessWindowStation();
    [DllImport("kernel32.dll")]
    public static extern uint GetCurrentThreadId();
    [DllImport("user32.dll")]
    public static extern IntPtr GetThreadDesktop(uint thread);
    [DllImport("user32.dll", CharSet=CharSet.Unicode, SetLastError=true)]
    public static extern bool GetUserObjectInformation(IntPtr handle, int index, System.Text.StringBuilder value, uint bytes, out uint needed);
    public static string ObjectName(IntPtr handle) {
        var text=new System.Text.StringBuilder(256);uint needed;
        return GetUserObjectInformation(handle,2,text,512,out needed)?text.ToString():"error:"+Marshal.GetLastWin32Error();
    }
}
'@
Add-Type -ReferencedAssemblies PresentationFramework,PresentationCore,WindowsBase,System.Xaml -TypeDefinition @'
using System;
using System.Threading;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Interop;
public static class RescueWpfProbe {
    static Window window;
    static ManualResetEvent ready = new ManualResetEvent(false);
    public static IntPtr Handle;
    public static string Error;
    public static void Start() {
        var thread = new Thread(() => {
            try {
                window = new Window { Title="Rescue owned UIA baseline", Width=320, Height=180 };
                window.Content = new Button { Content="Rescue native baseline button" };
                window.Show(); Handle = new WindowInteropHelper(window).Handle; ready.Set();
                System.Windows.Threading.Dispatcher.Run();
            } catch(Exception error) { Error=error.ToString(); ready.Set(); }
        });
        thread.SetApartmentState(ApartmentState.STA); thread.IsBackground=true; thread.Start();
        if(!ready.WaitOne(10000)) throw new Exception("Owned WPF baseline did not start");
    }
    public static void Stop() {
        if(window!=null) window.Dispatcher.BeginInvoke(new Action(() => {window.Close();System.Windows.Threading.Dispatcher.ExitAllFrames();}));
    }
}
'@
$cfg = Get-Content -LiteralPath $Config -Raw -Encoding UTF8 | ConvertFrom-Json
$script:owned = $null
$script:root = $null
$script:steps = [System.Collections.Generic.List[string]]::new()
function Nodes {
    $script:root = [System.Windows.Automation.AutomationElement]::FromHandle($script:owned.MainWindowHandle)
    $items = $script:root.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition)
    for ($index=0; $index -lt $items.Count; $index++) { Write-Output -NoEnumerate ($items.Item($index)) }
}
function Find-Control([string]$Name, [string]$Type = '') {
    foreach ($node in (Nodes)) {
        if ($node.Current.Name -ceq $Name -and (!$Type -or $node.Current.ControlType.ProgrammaticName -eq "ControlType.$Type")) { return $node }
    }
    return $null
}
function Await-Control([string]$Name, [string]$Type = '') {
    $until = [DateTime]::UtcNow.AddSeconds(30)
    do {
        $script:owned.Refresh()
        if ($script:owned.HasExited) { throw "Owned GUI exited: $($script:owned.ExitCode)" }
        $item = Find-Control $Name $Type
        if ($null -ne $item) { return $item }
        Start-Sleep -Milliseconds 100
    } while ([DateTime]::UtcNow -lt $until)
    throw "Missing accessible control: $Type / $Name"
}
function Invoke-Control([string]$Name) {
    $node = Await-Control $Name
    $pattern = $null
    if ($node.TryGetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern, [ref]$pattern)) { $pattern.Invoke() }
    elseif ($node.TryGetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern, [ref]$pattern)) { $pattern.Select() }
    elseif ($node.TryGetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern, [ref]$pattern)) { $pattern.Toggle() }
    else { throw "No activation pattern: $Name" }
    Start-Sleep -Milliseconds 200
    $script:steps.Add("invoke: $Name")
}
function Set-Field([string]$Name, [string]$Value) {
    $node = Await-Control $Name 'Edit'
    $pattern = $node.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern)
    if ($pattern.Current.IsReadOnly) { throw "Read-only edit: $Name" }
    $pattern.SetValue($Value)
    $until = [DateTime]::UtcNow.AddSeconds(10)
    do {
        Start-Sleep -Milliseconds 100
        $node = Await-Control $Name 'Edit'
        $pattern = $node.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern)
        if ($pattern.Current.Value -ceq $Value) { $script:steps.Add("set-value: $Name"); return }
    } while ([DateTime]::UtcNow -lt $until)
    throw "Edit did not preserve supplied Unicode value: $Name"
}
function Snapshot {
    $result = @()
    if ($null -ne $script:root) {
        foreach ($node in (Nodes)) {
            try {
                $c = $node.Current
                $result += @{name=$c.Name;type=$c.ControlType.ProgrammaticName;enabled=$c.IsEnabled;focusable=$c.IsKeyboardFocusable;offscreen=$c.IsOffscreen;bounds=@($c.BoundingRectangle.X,$c.BoundingRectangle.Y,$c.BoundingRectangle.Width,$c.BoundingRectangle.Height);patterns=@($node.GetSupportedPatterns() | ForEach-Object {$_.ProgrammaticName})}
            } catch {}
        }
    }
    return $result
}
try {
    [RescueWpfProbe]::Start()
    Start-Sleep -Milliseconds 1000
    $script:baseline = @{error=[RescueWpfProbe]::Error}
    try {
        $probe = [System.Windows.Automation.AutomationElement]::FromHandle([RescueWpfProbe]::Handle)
        $script:baseline.framework=$probe.Current.FrameworkId
        $script:baseline.children=$probe.FindAll([System.Windows.Automation.TreeScope]::Descendants,[System.Windows.Automation.Condition]::TrueCondition).Count
        $script:baseline.server_side=[RescueOwnedWindow]::UiaHasServerSideProvider([RescueWpfProbe]::Handle)
        $script:baseline.name=$probe.Current.Name
        $script:baseline.pid=$probe.Current.ProcessId
    } finally { [RescueWpfProbe]::Stop() }
    $script:owned = Start-Process -FilePath $cfg.binary -ArgumentList @('--language','en') -PassThru -RedirectStandardOutput (Join-Path $cfg.root 'stdout.txt') -RedirectStandardError (Join-Path $cfg.root 'stderr.txt')
    $until = [DateTime]::UtcNow.AddSeconds(30)
    do {
        Start-Sleep -Milliseconds 100
        $script:owned.Refresh()
        if ($script:owned.HasExited) { throw 'GUI exited before creating its window' }
    } while (!$script:owned.MainWindowHandle -and [DateTime]::UtcNow -lt $until)
    if (!$script:owned.MainWindowHandle) { throw 'No window from owned GUI' }
    # Window creation precedes renderer and AccessKit initialization. Do not
    # cache the default HWND provider while the actual provider is still loading.
    $until = [DateTime]::UtcNow.AddSeconds(30)
    do {
        Start-Sleep -Milliseconds 100
        $diagnostic = Get-Content -LiteralPath (Join-Path $cfg.root 'stderr.txt') -Raw -ErrorAction SilentlyContinue
    } while ($diagnostic -notmatch 'Rescue renderer:' -and [DateTime]::UtcNow -lt $until)
    if ($diagnostic -notmatch 'Rescue renderer:') {throw 'Renderer did not initialize before UIA inspection'}
    # AccessKit subclasses the window after the renderer starts. A UIA query
    # before that binds this client to the plain HWND proxy, whose tree stays
    # empty. Wait for the adapter's window property before the first UIA call.
    $adapterWait = [System.Diagnostics.Stopwatch]::StartNew()
    do {
        Start-Sleep -Milliseconds 100
        $script:owned.Refresh()
        if ($script:owned.HasExited) { throw "Owned GUI exited: $($script:owned.ExitCode)" }
        $adapter = [RescueOwnedWindow]::GetProp($script:owned.MainWindowHandle,'AccessKitAdapter').ToInt64()
    } while ($adapter -eq 0 -and $adapterWait.Elapsed.TotalSeconds -lt 30)
    if ($adapter -eq 0) { throw 'AccessKit adapter was not attached to the owned window' }
    $adapterMs = [int]$adapterWait.Elapsed.TotalMilliseconds
    Start-Sleep -Milliseconds 300
    $script:root = [System.Windows.Automation.AutomationElement]::FromHandle($script:owned.MainWindowHandle)
    if ($script:root.Current.ProcessId -ne $script:owned.Id) { throw 'Automation root belongs to another process' }
    # Probe only our own HWND. This records whether Windows reaches the native
    # provider, separately from a missing or not-yet-published widget tree.
    $script:provider = @{server_side=[RescueOwnedWindow]::UiaHasServerSideProvider($script:owned.MainWindowHandle);framework=$script:root.Current.FrameworkId;handle=$script:owned.MainWindowHandle.ToInt64()}
    $messageResult = [UIntPtr]::Zero
    $message = [RescueOwnedWindow]::SendMessageTimeout($script:owned.MainWindowHandle,0x3d,[UIntPtr]::Zero,[IntPtr](-25),2,2000,[ref]$messageResult)
    $script:provider.getobject_completed=$message -ne [IntPtr]::Zero
    $script:provider.getobject_result=$messageResult.ToUInt64()
    $script:provider.accesskit_property=[RescueOwnedWindow]::GetProp($script:owned.MainWindowHandle,'AccessKitAdapter').ToInt64()
    $script:provider.baseline=$script:baseline
    $script:provider.adapter_wait_ms=$adapterMs
    $script:provider.session=@{client=(Get-Process -Id $PID).SessionId;gui=$script:owned.SessionId;console=[RescueOwnedWindow]::WTSGetActiveConsoleSessionId();station=[RescueOwnedWindow]::ObjectName([RescueOwnedWindow]::GetProcessWindowStation());desktop=[RescueOwnedWindow]::ObjectName([RescueOwnedWindow]::GetThreadDesktop([RescueOwnedWindow]::GetCurrentThreadId()))}
    $description = [System.Windows.Automation.AutomationProperty]::LookupById(30107)
    if ($null -ne $description) {$script:provider.description=$script:root.GetCurrentPropertyValue($description)}
    Invoke-Control 'Saved image'
    Set-Field 'Image file' $cfg.source
    Invoke-Control 'Save as ZIP'
    Set-Field 'Destination' $cfg.destination
    Invoke-Control 'Start scan'
    $null = Await-Control 'Search results' 'Edit'
    Set-Field 'Search results' '.pdf'
    $null = Await-Control $cfg.pdf
    Invoke-Control 'Select visible candidates'
    Set-Field 'Search results' $cfg.query
    $null = Await-Control $cfg.text
    Invoke-Control 'Select visible candidates'
    $script:selection = Snapshot
    Invoke-Control 'Recover selected files'
    $until = [DateTime]::UtcNow.AddSeconds(30)
    do {
        Start-Sleep -Milliseconds 100
        $saved = Find-Control 'ZIP: Saved / 2 files'
    } while ($null -eq $saved -and [DateTime]::UtcNow -lt $until)
    if ($null -eq $saved) { throw 'No two-file completion status in the accessibility tree' }
    if (!(Test-Path -LiteralPath $cfg.destination)) { throw 'The GUI reported completion without a ZIP' }
    @{status='passed';process_id=$script:owned.Id;steps=@($script:steps);selection_tree=$script:selection;final_tree=(Snapshot);native_input_method_tested=$false;scope='Native Windows UIA Invoke/Value patterns, owned regular image and Unicode query; real IME and keyboard input are separate checks.'} | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath $cfg.result -Encoding UTF8
} catch {
    $failure = $_
    $evidence = @{status='failed';error=$failure.Exception.ToString();steps=@($script:steps);provider=$script:provider}
    try {
        $evidence.window_title=$script:owned.MainWindowTitle
        $evidence.root_name=$script:root.Current.Name
        $evidence.root_class=$script:root.Current.ClassName
        $evidence.tree=@(Snapshot)
        $evidence.provider.late_accesskit_property=[RescueOwnedWindow]::GetProp($script:owned.MainWindowHandle,'AccessKitAdapter').ToInt64()
    } catch { $evidence.diagnostic_error=$_.Exception.ToString() }
    $evidence | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath $cfg.result -Encoding UTF8
    throw $failure
} finally {
    if ($null -ne $script:owned) {
        $script:owned.Refresh()
        if (!$script:owned.HasExited) { $null = $script:owned.CloseMainWindow(); if (!$script:owned.WaitForExit(5000)) {$script:owned.Kill();$script:owned.WaitForExit()} }
    }
}
