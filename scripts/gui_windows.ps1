# Own-process UI Automation on a disposable native Windows runner. No host
# storage, external windows, or user files are selected.
param([Parameter(Mandatory=$true)][string]$Config)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
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
    Start-Sleep -Milliseconds 500
    $script:root = [System.Windows.Automation.AutomationElement]::FromHandle($script:owned.MainWindowHandle)
    if ($script:root.Current.ProcessId -ne $script:owned.Id) { throw 'Automation root belongs to another process' }
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
    $evidence = @{status='failed';error=$failure.Exception.ToString();steps=@($script:steps)}
    try {
        $evidence.window_title=$script:owned.MainWindowTitle
        $evidence.root_name=$script:root.Current.Name
        $evidence.root_class=$script:root.Current.ClassName
        $evidence.tree=@(Snapshot)
    } catch { $evidence.diagnostic_error=$_.Exception.ToString() }
    $evidence | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath $cfg.result -Encoding UTF8
    throw $failure
} finally {
    if ($null -ne $script:owned) {
        $script:owned.Refresh()
        if (!$script:owned.HasExited) { $null = $script:owned.CloseMainWindow(); if (!$script:owned.WaitForExit(5000)) {$script:owned.Kill();$script:owned.WaitForExit()} }
    }
}
