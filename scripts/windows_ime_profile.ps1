# Disposable runner fixture: keep the user language profile stable across GUI
# processes. Restore the exact language order, input methods and user options.
param([Parameter(Mandatory=$true)][ValidateSet('Prepare','Restore')][string]$Action,
      [string]$State='ime-profile-state.json')
$ErrorActionPreference='Stop'
if ($env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_ENVIRONMENT -ne 'github-hosted') {throw 'IME profile fixture requires a disposable GitHub-hosted VM'}
function Read-Languages {
    return @(foreach ($language in (Get-WinUserLanguageList)) {
        @{tag=$language.LanguageTag;tips=@($language.InputMethodTips);handwriting=$language.Handwriting;spellchecking=$language.Spellchecking}
    })
}
function Set-Languages($Records) {
    if (!$Records.Count) {throw 'Cannot restore an empty language list'}
    $list=New-WinUserLanguageList $Records[0].tag
    for ($i=1;$i -lt $Records.Count;$i++) {$list.Add($Records[$i].tag)}
    for ($i=0;$i -lt $Records.Count;$i++) {
        $list[$i].InputMethodTips.Clear()
        foreach ($tip in $Records[$i].tips) {$list[$i].InputMethodTips.Add($tip)}
        $list[$i].Handwriting=$Records[$i].handwriting
        $list[$i].Spellchecking=$Records[$i].spellchecking
    }
    Set-WinUserLanguageList $list -Force
}
$tip='0411:{03B5835F-F03C-411B-9CE2-AA23E1171E36}{A76C93D9-5523-4E90-AAFA-4DB112F9AC76}'
if ($Action -eq 'Prepare') {
    if (Test-Path -LiteralPath $State) {throw 'IME profile fixture state already exists'}
    $record=@{before=@(Read-Languages);prepared=$false;restored=$false}
    $record|ConvertTo-Json -Depth 8|Set-Content -LiteralPath $State -Encoding UTF8
    $languages=Get-WinUserLanguageList
    $japanese=$null
    foreach ($language in $languages) {if ($language.LanguageTag -match '^ja($|-)') {$japanese=$language}}
    if ($null -eq $japanese) {$languages.Add('ja-JP');$japanese=$languages[$languages.Count-1]}
    if (!$japanese.InputMethodTips.Contains($tip)) {$japanese.InputMethodTips.Add($tip)}
    Set-WinUserLanguageList $languages -Force
    $timer=[System.Diagnostics.Stopwatch]::StartNew()
    do {
        Start-Sleep -Milliseconds 100
        $record.configured=@(Read-Languages)
        $record.prepared=@($record.configured|Where-Object {$_.tag -match '^ja($|-)' -and $_.tips -contains $tip}).Count -eq 1
    } while (!$record.prepared -and $timer.Elapsed.TotalSeconds -lt 10)
    $record.prepare_ms=[int]$timer.Elapsed.TotalMilliseconds
    $record|ConvertTo-Json -Depth 8|Set-Content -LiteralPath $State -Encoding UTF8
    if (!$record.prepared) {throw 'Japanese user language profile did not register before GUI launch'}
} else {
    if (!(Test-Path -LiteralPath $State)) {return}
    $record=Get-Content -LiteralPath $State -Raw -Encoding UTF8|ConvertFrom-Json
    Set-Languages @($record.before)
    $after=@(Read-Languages)
    $before=@($record.before)
    $matches=$after.Count -eq $before.Count
    for ($i=0;$matches -and $i -lt $before.Count;$i++) {
        $matches=$after[$i].tag -ceq $before[$i].tag -and
            ($after[$i].tips -join '|') -ceq ($before[$i].tips -join '|') -and
            $after[$i].handwriting -eq $before[$i].handwriting -and
            $after[$i].spellchecking -eq $before[$i].spellchecking
    }
    $record|Add-Member -NotePropertyName after -NotePropertyValue $after -Force
    $record.restored=$matches
    $record|ConvertTo-Json -Depth 8|Set-Content -LiteralPath $State -Encoding UTF8
    if (!$matches) {throw 'Original user language profile was not restored'}
}
