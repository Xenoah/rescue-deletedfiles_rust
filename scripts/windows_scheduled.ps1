# Own temporary task on a disposable hosted VM, using the current interactive
# identity at Task Scheduler's least-privilege run level. No password/policy edit.
param([Parameter(Mandatory=$true)][string]$Config)
$ErrorActionPreference='Stop'
if ($env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_ENVIRONMENT -ne 'github-hosted') {throw 'Disposable hosted VM required'}
$c=Get-Content -LiteralPath $Config -Raw -Encoding UTF8|ConvertFrom-Json
$service=New-Object -ComObject Schedule.Service
$service.Connect()
$folder=$service.GetFolder('\')
$name='Rescue-owned-gui-'+[Guid]::NewGuid().ToString('N')
$registered=$null
try {
    $definition=$service.NewTask(0)
    $definition.RegistrationInfo.Description='Owned Rescue native GUI test; removed in finally'
    $definition.Principal.UserId=[System.Security.Principal.WindowsIdentity]::GetCurrent().Name
    $definition.Principal.LogonType=3
    $definition.Principal.RunLevel=0
    $definition.Settings.ExecutionTimeLimit='PT3M'
    $definition.Settings.DisallowStartIfOnBatteries=$false
    $definition.Settings.StopIfGoingOnBatteries=$false
    $action=$definition.Actions.Create(0)
    $action.Path=$c.executable
    $action.Arguments=$c.arguments
    $action.WorkingDirectory=$c.root
    $registered=$folder.RegisterTaskDefinition($name,$definition,2,$null,$null,3,$null)
    $instance=$registered.Run($null)
    $until=[DateTime]::UtcNow.AddSeconds(180)
    do {
        Start-Sleep -Milliseconds 200
        $instance.Refresh()
        if ([DateTime]::UtcNow -gt $until) {throw 'Owned scheduled GUI timed out'}
    } while ($instance.State -eq 2 -or $instance.State -eq 4)
    if ($registered.LastTaskResult -ne 0) {throw "Owned scheduled launcher exited: $($registered.LastTaskResult)"}
} finally {
    if ($null -ne $registered) {$registered.Stop(0);$folder.DeleteTask($name,0)}
}
