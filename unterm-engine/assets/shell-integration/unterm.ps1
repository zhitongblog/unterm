# Unterm shell integration for PowerShell: OSC 133 prompt and command marks,
# and OSC 7 for the working directory. Wraps whatever prompt the profile set.
if (-not $global:__UntermIntegration) {
    $global:__UntermIntegration = $true
    $global:__UntermOriginalPrompt = $function:prompt
    $global:__UntermFirstPrompt = $true

    $global:__UntermLastError = if ($global:Error.Count) { $global:Error[0] } else { $null }

    function global:prompt {
        $ok = $?
        # A cmdlet that failed left a new error record; a program that failed
        # left only its exit code. $LASTEXITCODE alone would be a stale one
        # from some earlier program whenever a cmdlet fails.
        $newest = if ($global:Error.Count) { $global:Error[0] } else { $null }
        $cmdletFailed = $null -ne $newest -and -not [object]::ReferenceEquals($newest, $global:__UntermLastError)
        $global:__UntermLastError = $newest
        $code = if ($ok) { 0 } elseif (-not $cmdletFailed -and $global:LASTEXITCODE) { $global:LASTEXITCODE } else { 1 }
        $esc = [char]27
        $bel = [char]7
        $marks = ''
        if (-not $global:__UntermFirstPrompt) { $marks += "$esc]133;D;$code$bel" }
        $global:__UntermFirstPrompt = $false
        $location = $executionContext.SessionState.Path.CurrentLocation
        if ($location.Provider.Name -eq 'FileSystem') {
            $path = $location.ProviderPath -replace '\\', '/'
            if (-not $path.StartsWith('/')) { $path = '/' + $path }
            $marks += "$esc]7;file://$env:COMPUTERNAME$path$bel"
        }
        $marks += "$esc]133;A$bel"
        $body = if ($global:__UntermOriginalPrompt) { & $global:__UntermOriginalPrompt } else { "PS $($location)> " }
        "$marks$body$esc]133;B$bel"
    }

    # Mark where a command starts running -- only when Enter still does what
    # PSReadLine ships with, so a user's own binding is left alone. `-Bound`
    # rather than `-Chord`: Windows PowerShell 5.1 ships PSReadLine 2.0,
    # which has no `-Chord`.
    if (Get-Module -Name PSReadLine) {
        $enter = Get-PSReadLineKeyHandler -Bound -ErrorAction SilentlyContinue |
            Where-Object { $_.Key -eq 'Enter' } | Select-Object -First 1
        if ($enter -and $enter.Function -eq 'AcceptLine') {
            Set-PSReadLineKeyHandler -Chord Enter -ScriptBlock {
                [Microsoft.PowerShell.PSConsoleReadLine]::AcceptLine()
                [Console]::Write("$([char]27)]133;C$([char]7)")
            }
        }
    }
}
