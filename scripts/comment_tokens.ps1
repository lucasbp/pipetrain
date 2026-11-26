$ErrorActionPreference = 'Stop'
[Console]::InputEncoding = [Text.UTF8Encoding]::new($false)
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
$tokens = $null
$diagnostics = $null
$null = [Management.Automation.Language.Parser]::ParseInput(
    [Console]::In.ReadToEnd(), [ref]$tokens, [ref]$diagnostics
)
$pending = [Collections.Generic.Queue[object]]::new()
foreach ($token in $tokens) { $pending.Enqueue($token) }
$result = while ($pending.Count) {
    $token = $pending.Dequeue()
    if ($token.Kind -eq 'Comment') {
        @{line = $token.Extent.StartLineNumber; text = $token.Text}
    }
    foreach ($nested in $token.NestedTokens) { $pending.Enqueue($nested) }
}
@{
    comments = @($result | Where-Object { $_ })
    errors = @($diagnostics | ForEach-Object {
        @{line = $_.Extent.StartLineNumber; message = $_.Message}
    })
} | ConvertTo-Json -Compress -Depth 4
