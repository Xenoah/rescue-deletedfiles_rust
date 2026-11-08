# Own-process UI Automation on a disposable native Windows runner. No host
# storage, external windows, or user files are selected.
param([Parameter(Mandatory=$true)][string]$Config)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
Add-Type -AssemblyName PresentationFramework, PresentationCore, WindowsBase, System.Xaml
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
[ComImport,Guid("71C6E74C-0F28-11D8-A82A-00065B84435C"),InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
public interface RescueInputProfileManager {
    void ActivateProfile(uint type,ushort language,ref Guid service,ref Guid profile,IntPtr layout,uint flags);
}
public static class RescueOwnedWindow {
    [DllImport("advapi32.dll", SetLastError=true)]
    static extern bool OpenProcessToken(IntPtr process, uint access, out IntPtr token);
    [DllImport("advapi32.dll", SetLastError=true)]
    static extern bool GetTokenInformation(IntPtr token, int kind, out uint value, uint size, out uint needed);
    [DllImport("kernel32.dll")]
    static extern bool CloseHandle(IntPtr handle);
    public static uint TokenValue(IntPtr process, int kind) {
        IntPtr token;
        if(!OpenProcessToken(process,8,out token)) throw new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error());
        try {uint value,needed; if(!GetTokenInformation(token,kind,out value,4,out needed)) throw new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error());return value;}
        finally {CloseHandle(token);}
    }
    [DllImport("advapi32.dll", SetLastError=true)]
    static extern bool DuplicateToken(IntPtr token, int level, out IntPtr copy);
    [DllImport("advapi32.dll", SetLastError=true)]
    static extern bool CheckTokenMembership(IntPtr token, IntPtr sid, out bool member);
    [DllImport("advapi32.dll", CharSet=CharSet.Unicode, SetLastError=true)]
    static extern bool ConvertStringSidToSid(string text, out IntPtr sid);
    [DllImport("advapi32.dll", EntryPoint="GetTokenInformation", SetLastError=true)]
    static extern bool GetTokenBuffer(IntPtr token, int kind, IntPtr buffer, uint size, out uint needed);
    [DllImport("advapi32.dll")] static extern IntPtr GetSidSubAuthorityCount(IntPtr sid);
    [DllImport("advapi32.dll")] static extern IntPtr GetSidSubAuthority(IntPtr sid,uint index);
    [DllImport("kernel32.dll")] static extern IntPtr LocalFree(IntPtr memory);
    public static bool AdministratorEnabled(IntPtr process) {
        IntPtr token=IntPtr.Zero,copy=IntPtr.Zero,sid=IntPtr.Zero;
        try {
            if(!OpenProcessToken(process,10,out token) || !DuplicateToken(token,2,out copy) || !ConvertStringSidToSid("S-1-5-32-544",out sid)) throw new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error());
            bool member; if(!CheckTokenMembership(copy,sid,out member)) throw new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error()); return member;
        } finally {if(copy!=IntPtr.Zero)CloseHandle(copy);if(token!=IntPtr.Zero)CloseHandle(token);if(sid!=IntPtr.Zero)LocalFree(sid);}
    }
    public static uint Integrity(IntPtr process) {
        IntPtr token=IntPtr.Zero,buffer=Marshal.AllocHGlobal(256);
        try {
            uint needed;
            if(!OpenProcessToken(process,8,out token) || !GetTokenBuffer(token,25,buffer,256,out needed)) throw new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error());
            IntPtr sid=Marshal.ReadIntPtr(buffer);byte count=Marshal.ReadByte(GetSidSubAuthorityCount(sid));
            return (uint)Marshal.ReadInt32(GetSidSubAuthority(sid,(uint)count-1));
        } finally {if(token!=IntPtr.Zero)CloseHandle(token);Marshal.FreeHGlobal(buffer);}
    }
    [DllImport("user32.dll", CharSet=CharSet.Unicode, SetLastError=true)]
    static extern IntPtr LoadKeyboardLayout(string name,uint flags);
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr hwnd,IntPtr process);
    [DllImport("user32.dll")] static extern IntPtr GetKeyboardLayout(uint thread);
    [DllImport("imm32.dll")] static extern bool ImmIsIME(IntPtr layout);
    [DllImport("imm32.dll", CharSet=CharSet.Unicode)] static extern uint ImmGetDescription(IntPtr layout,System.Text.StringBuilder text,uint size);
    [DllImport("imm32.dll")] static extern IntPtr ImmGetDefaultIMEWnd(IntPtr hwnd);
    static IntPtr previousImeLayout=IntPtr.Zero;
    public static bool ImeRestored=false;
    public static void RestoreJapaneseIme(IntPtr hwnd) {
        if(previousImeLayout==IntPtr.Zero || ImeRestored)return;
        Guid empty=Guid.Empty;
        var manager=(RescueInputProfileManager)Activator.CreateInstance(Type.GetTypeFromCLSID(new Guid("33C53A50-F456-4884-B049-85FD643ECFED")));
        try {manager.ActivateProfile(2,(ushort)(previousImeLayout.ToInt64()&65535),ref empty,ref empty,previousImeLayout,0x20000004);}
        finally {Marshal.ReleaseComObject(manager);}
        // TSF profile activation alone need not change an inactive window's
        // thread HKL. Request the saved layout on this owned HWND as well.
        SetForegroundWindow(hwnd);
        // Windows posts WM_INPUTLANGCHANGEREQUEST; a synchronous SendMessage
        // can deadlock TSF cross-thread callbacks while this driver is waiting.
        if(!PostMessage(hwnd,0x50,UIntPtr.Zero,previousImeLayout))
            throw new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error());
        for(int i=0;i<30;i++) {
            System.Threading.Thread.Sleep(100);
            if(GetKeyboardLayout(GetWindowThreadProcessId(hwnd,IntPtr.Zero))==previousImeLayout) {ImeRestored=true;return;}
        }
        throw new Exception("Owned GUI input layout was not restored: expected="+previousImeLayout.ToInt64()+", actual="+GetKeyboardLayout(GetWindowThreadProcessId(hwnd,IntPtr.Zero)).ToInt64());
    }
    public static string EnableJapaneseIme(IntPtr hwnd,string serviceId,string profileId) {
        previousImeLayout=GetKeyboardLayout(GetWindowThreadProcessId(hwnd,IntPtr.Zero));
        // Modern Microsoft IME is a TSF profile; the old E0010411 HKL can
        // silently return the English fallback even with Japanese FOD present.
        Guid service=new Guid(serviceId),profile=new Guid(profileId);
        var manager=(RescueInputProfileManager)Activator.CreateInstance(Type.GetTypeFromCLSID(new Guid("33C53A50-F456-4884-B049-85FD643ECFED")));
        try {manager.ActivateProfile(1,0x411,ref service,ref profile,IntPtr.Zero,0x20000005);}
        finally {Marshal.ReleaseComObject(manager);}
        IntPtr layout=IntPtr.Zero;
        for(int i=0;i<30;i++) {
            System.Threading.Thread.Sleep(100);
            layout=GetKeyboardLayout(GetWindowThreadProcessId(hwnd,IntPtr.Zero));
            if((layout.ToInt64()&65535)==0x411)break;
        }
        if((layout.ToInt64()&65535)!=0x411)throw new Exception("Japanese TSF profile did not activate for the owned GUI: "+layout.ToInt64());
        var description=new System.Text.StringBuilder(256);ImmGetDescription(layout,description,256);
        IntPtr ime=ImmGetDefaultIMEWnd(hwnd);UIntPtr result;
        if(ime==IntPtr.Zero)throw new Exception("Owned GUI has no native IME window");
        SendMessageTimeout(ime,0x283,new UIntPtr(6),new IntPtr(1),2,5000,out result);
        SendMessageTimeout(ime,0x283,new UIntPtr(2),new IntPtr(0x19),2,5000,out result);
        return description.ToString()+" / TSF="+serviceId+"/"+profileId+" / HKL="+layout.ToInt64()+" / IMM="+ImmIsIME(layout);
    }
    public static void RomanKeys(string value) {foreach(char c in value) Tap((ushort)Char.ToUpperInvariant(c));}
    [StructLayout(LayoutKind.Sequential)]
    public struct KeyboardInput { public ushort key, scan; public uint flags, time; public UIntPtr extra; }
    [StructLayout(LayoutKind.Explicit, Size=40)]
    public struct Input { [FieldOffset(0)] public uint type; [FieldOffset(8)] public KeyboardInput keyboard; }
    [DllImport("user32.dll", SetLastError=true)]
    static extern uint SendInput(uint count, Input[] input, int size);
    [DllImport("user32.dll")]
    public static extern bool SetForegroundWindow(IntPtr hwnd);
    [DllImport("user32.dll")]
    public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll", SetLastError=true)]
    static extern bool PostMessage(IntPtr hwnd,uint message,UIntPtr wparam,IntPtr lparam);
    public static void Key(ushort key, bool up, bool unicode) {
        var input=new Input {type=1, keyboard=new KeyboardInput {key=unicode?(ushort)0:key, scan=unicode?key:(ushort)0, flags=(up?2u:0u)|(unicode?4u:0u)}};
        if(SendInput(1,new[]{input},Marshal.SizeOf(typeof(Input)))!=1) throw new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error());
    }
    public static void Tap(ushort key) {Key(key,false,false);Key(key,true,false);}
    public static void Text(string value) {foreach(char c in value) {Key(c,false,true);Key(c,true,true);}}
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
    if ($cfg.keyboard) {
        Focus-Owned $node
        [RescueOwnedWindow]::Tap(0x20)
        Start-Sleep -Milliseconds 200
        $script:steps.Add("keyboard-space: $Name")
        return
    }
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
    if ($cfg.keyboard) {
        Focus-Owned $node
        [RescueOwnedWindow]::Key(0x11,$false,$false)
        [RescueOwnedWindow]::Tap(0x41)
        [RescueOwnedWindow]::Key(0x11,$true,$false)
        [RescueOwnedWindow]::Text($Value)
    } else { $pattern.SetValue($Value) }
    $until = [DateTime]::UtcNow.AddSeconds(10)
    do {
        Start-Sleep -Milliseconds 100
        $node = Await-Control $Name 'Edit'
        $pattern = $node.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern)
        if ($pattern.Current.Value -ceq $Value) { $script:steps.Add("$(if ($cfg.keyboard) {'keyboard-unicode'} else {'set-value'}): $Name"); return }
    } while ([DateTime]::UtcNow -lt $until)
    throw "Edit did not preserve supplied Unicode value: $Name"
}
function Focus-Owned($Node) {
    $null = [RescueOwnedWindow]::SetForegroundWindow($script:owned.MainWindowHandle)
    $Node.SetFocus()
    $until=[DateTime]::UtcNow.AddSeconds(3)
    do {
        Start-Sleep -Milliseconds 50
        $focused=[System.Windows.Automation.AutomationElement]::FocusedElement
    } while (![System.Windows.Automation.Automation]::Compare($focused,$Node) -and [DateTime]::UtcNow -lt $until)
    if (![System.Windows.Automation.Automation]::Compare($focused,$Node)) {throw "Focus did not reach requested control: $($Node.Current.Name), observed $($focused.Current.Name)"}
    if ([RescueOwnedWindow]::GetForegroundWindow() -ne $script:owned.MainWindowHandle) {throw 'Owned GUI did not acquire keyboard foreground'}
    if ([System.Windows.Automation.AutomationElement]::FocusedElement.Current.ProcessId -ne $script:owned.Id) {throw 'Keyboard focus belongs to another process'}
}
function Check-TabNavigation {
    # Search appears before the first asynchronous indexed page is ready.
    # Wait for stable enabled controls so Tab and Shift+Tab traverse the same
    # sequence instead of racing a newly-enabled selection button.
    $until=[DateTime]::UtcNow.AddSeconds(10)
    do {$ready=Await-Control 'Select visible candidates';Start-Sleep -Milliseconds 50} while (!$ready.Current.IsEnabled -and [DateTime]::UtcNow -lt $until)
    if (!$ready.Current.IsEnabled) {throw 'Result page did not become ready for keyboard navigation'}
    $node=Await-Control 'Search results' 'Edit'; Focus-Owned $node
    [RescueOwnedWindow]::Tap(9)
    $until=[DateTime]::UtcNow.AddSeconds(3)
    do {
        Start-Sleep -Milliseconds 50
        $forward=[System.Windows.Automation.AutomationElement]::FocusedElement.Current
    } while ($forward.Name -eq 'Search results' -and [DateTime]::UtcNow -lt $until)
    if ($forward.ProcessId -ne $script:owned.Id -or $forward.Name -eq 'Search results' -or !$forward.Name) {throw "Tab did not reach a named owned control: name=$($forward.Name), type=$($forward.ControlType.ProgrammaticName), process=$($forward.ProcessId)"}
    [RescueOwnedWindow]::Key(0x10,$false,$false); [RescueOwnedWindow]::Tap(9); [RescueOwnedWindow]::Key(0x10,$true,$false)
    $until=[DateTime]::UtcNow.AddSeconds(3)
    do {Start-Sleep -Milliseconds 50;$backward=[System.Windows.Automation.AutomationElement]::FocusedElement.Current} while ($backward.Name -ne 'Search results' -and [DateTime]::UtcNow -lt $until)
    if ($backward.Name -ne 'Search results') {throw "Shift+Tab did not return to search: forward=$($forward.Name), backward=$($backward.Name)"}
    $script:steps.Add("keyboard-tab-and-shift-tab: Search results / $($forward.Name)")
}
function Set-ImeQuery {
    # Register an already-installed Microsoft IME in this disposable user's
    # language list. Restore the original list in finally, even on failure.
    $script:priorLanguages=Get-WinUserLanguageList
    $service='{03B5835F-F03C-411B-9CE2-AA23E1171E36}';$profile='{A76C93D9-5523-4E90-AAFA-4DB112F9AC76}'
    $installed="HKLM:\SOFTWARE\Microsoft\CTF\TIP\$service\LanguageProfile\0x00000411\$profile"
    if (!(Test-Path -LiteralPath $installed)) {throw 'The installed Japanese Microsoft IME TSF registration is missing'}
    $languages=New-WinUserLanguageList 'en-US';$languages.Add('ja-JP')
    $languages[1].InputMethodTips.Clear();$languages[1].InputMethodTips.Add("0411:$service$profile")
    Set-WinUserLanguageList $languages -Force
    $node=Await-Control 'Search results' 'Edit';Focus-Owned $node
    [RescueOwnedWindow]::Key(0x11,$false,$false);[RescueOwnedWindow]::Tap(0x41);[RescueOwnedWindow]::Key(0x11,$true,$false);[RescueOwnedWindow]::Tap(8)
    $tips=@((Get-WinUserLanguageList|Where-Object LanguageTag -eq 'ja-JP').InputMethodTips)
    $script:imeDiagnostic=@{tips=$tips;installed_profile=$installed;requested_tip="0411:$service$profile"}
    $description=[RescueOwnedWindow]::EnableJapaneseIme($script:owned.MainWindowHandle,$service,$profile)
    Start-Sleep -Milliseconds 500
    [RescueOwnedWindow]::RomanKeys('torikeshi')
    # Wait for the actual preedit, not an assumed keyboard/TSF processing time.
    # Otherwise Escape may be sent before there is any composition to cancel.
    $until=[DateTime]::UtcNow.AddSeconds(5)
    do {
        Start-Sleep -Milliseconds 100
        $preedit=(Await-Control 'Search results' 'Edit').GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value
    } while ($preedit -cne 'とりけし' -and [DateTime]::UtcNow -lt $until)
    $script:imeDiagnostic.cancel_start_value=$preedit
    if ($preedit -cne 'とりけし') {throw "Native IME cancellation preedit was not ready: $preedit"}
    $cancelTrace=@()
    for ($press=1;$press -le 2;$press++) {
        [RescueOwnedWindow]::Tap(27);Start-Sleep -Milliseconds 400
        $edit=Await-Control 'Search results' 'Edit'
        $cancelTrace+=@{escape=$press;value=$edit.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value;focus=[System.Windows.Automation.AutomationElement]::FocusedElement.Current.Name}
    }
    $script:imeDiagnostic.cancel_trace=$cancelTrace
    Start-Sleep -Milliseconds 300
    $value=(Await-Control 'Search results' 'Edit').GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value
    if ($value -ne '') {throw "Native IME cancellation committed unexpected text: $value"}
    # A second Escape can legitimately leave the edit after composition ends.
    # Check cancellation above before restoring focus for the next conversion.
    Focus-Owned (Await-Control 'Search results' 'Edit')
    [RescueOwnedWindow]::RomanKeys('genbun');Start-Sleep -Milliseconds 400;[RescueOwnedWindow]::Tap(0x20);Start-Sleep -Milliseconds 400;[RescueOwnedWindow]::Tap(13)
    $until=[DateTime]::UtcNow.AddSeconds(5)
    do {
        Start-Sleep -Milliseconds 100
        $value=(Await-Control 'Search results' 'Edit').GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value
    } while ($value -cne $cfg.query -and [DateTime]::UtcNow -lt $until)
    if ($value -cne $cfg.query) {throw "Native IME conversion differed: expected=$($cfg.query), actual=$value, IME=$description"}
    $script:imeProof=@{description=$description;cancelled_roman_keys='torikeshi';committed_roman_keys='genbun';committed_value=$value;unicode_injection_used=$false;cancel_escape_presses=2;cancel_start_value=$preedit;cancel_trace=$cancelTrace;os_version=[Environment]::OSVersion.Version.ToString()}
    $script:steps.Add('native-ime: cancel prediction and composition / convert genbun / commit '+$cfg.query)
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
    if ($cfg.owned_pid) {
        $script:owned=Get-Process -Id $cfg.owned_pid
        if ($script:owned.Path -ine $cfg.binary) {throw 'Supplied process is not the owned GUI binary'}
    } else {
    $script:owned = Start-Process -FilePath $cfg.binary -ArgumentList @('--language','en') -PassThru -RedirectStandardOutput (Join-Path $cfg.root 'stdout.txt') -RedirectStandardError (Join-Path $cfg.root 'stderr.txt')
    }
    $script:guiToken=@{elevated=[RescueOwnedWindow]::TokenValue($script:owned.Handle,20);elevation_type=[RescueOwnedWindow]::TokenValue($script:owned.Handle,18);administrator_enabled=[RescueOwnedWindow]::AdministratorEnabled($script:owned.Handle);integrity=[RescueOwnedWindow]::Integrity($script:owned.Handle)}
    if ($cfg.require_nonadmin -and ($script:guiToken.elevated -ne 0 -or $script:guiToken.administrator_enabled -or $script:guiToken.integrity -ne 8192)) {throw "Actual GUI is not non-elevated/medium/non-admin: $($script:guiToken|ConvertTo-Json -Compress)"}
    if ($cfg.require_uac -and $script:guiToken.elevation_type -ne 3) {throw "Actual GUI is not UAC-linked: $($script:guiToken|ConvertTo-Json -Compress)"}
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
    if ($cfg.keyboard) { Check-TabNavigation }
    Set-Field 'Search results' '.pdf'
    $null = Await-Control $cfg.pdf
    Invoke-Control 'Select visible candidates'
    if ($cfg.ime) {Set-ImeQuery} else {Set-Field 'Search results' $cfg.query}
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
    Add-Type -AssemblyName System.Drawing,System.Windows.Forms
    $windowBounds=$script:root.Current.BoundingRectangle
    $bounds=[System.Drawing.Rectangle]::Intersect([System.Drawing.Rectangle]::new([int]$windowBounds.X,[int]$windowBounds.Y,[int]$windowBounds.Width,[int]$windowBounds.Height),[System.Windows.Forms.Screen]::FromHandle($script:owned.MainWindowHandle).Bounds)
    $bitmap=[System.Drawing.Bitmap]::new([int]$bounds.Width,[int]$bounds.Height)
    $graphics=[System.Drawing.Graphics]::FromImage($bitmap)
    try {
        $graphics.CopyFromScreen([int]$bounds.X,[int]$bounds.Y,0,0,$bitmap.Size)
        $bitmap.Save((Join-Path $cfg.root 'windows-gui-complete.png'),[System.Drawing.Imaging.ImageFormat]::Png)
    } finally {$graphics.Dispose();$bitmap.Dispose()}
    if ($cfg.ime) {[RescueOwnedWindow]::RestoreJapaneseIme($script:owned.MainWindowHandle)}
    @{status='passed';input_layout_restored=[RescueOwnedWindow]::ImeRestored;process_id=$script:owned.Id;gui_token=$script:guiToken;steps=@($script:steps);selection_tree=$script:selection;final_tree=(Snapshot);native_input_method_tested=[bool]$script:imeProof;native_ime=$script:imeProof;native_keyboard_input_tested=[bool]$cfg.keyboard;scope='Native Windows UIA and Invoke/Value or SendInput keyboard actions. native_ime records actual Roman-key conversion only when requested; Unicode injection alone is not an IME test.'} | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath $cfg.result -Encoding UTF8
} catch {
    $failure = $_
    $evidence = @{status='failed';gui_token=$script:guiToken;native_ime_debug=$script:imeDiagnostic;error=$failure.Exception.ToString();steps=@($script:steps);provider=$script:provider}
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
    try {
        if ($script:priorLanguages) {
            Set-WinUserLanguageList $script:priorLanguages -Force
            if ($null -ne $script:owned -and !$script:owned.HasExited) {[RescueOwnedWindow]::RestoreJapaneseIme($script:owned.MainWindowHandle)}
        }
    } finally {
        if ($null -ne $script:owned) {
            $script:owned.Refresh()
            if (!$script:owned.HasExited) { $null = $script:owned.CloseMainWindow(); if (!$script:owned.WaitForExit(5000)) {$script:owned.Kill();$script:owned.WaitForExit()} }
        }
    }
}
