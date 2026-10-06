
using namespace System.Management.Automation
using namespace System.Management.Automation.Language

Register-ArgumentCompleter -Native -CommandName 'srvm' -ScriptBlock {
    param($wordToComplete, $commandAst, $cursorPosition)

    $commandElements = $commandAst.CommandElements
    $command = @(
        'srvm'
        for ($i = 1; $i -lt $commandElements.Count; $i++) {
            $element = $commandElements[$i]
            if ($element -isnot [StringConstantExpressionAst] -or
                $element.StringConstantType -ne [StringConstantType]::BareWord -or
                $element.Value.StartsWith('-') -or
                $element.Value -eq $wordToComplete) {
                break
        }
        $element.Value
    }) -join ';'

    $completions = @(switch ($command) {
        'srvm' {
            [CompletionResult]::new('--port', '--port', [CompletionResultType]::ParameterName, 'Start port search at N (0 chooses a free port)')
            [CompletionResult]::new('--select', '--select', [CompletionResultType]::ParameterName, 'Run one app: index, qualified id, or a unique name')
            [CompletionResult]::new('--dry-run', '--dry-run', [CompletionResultType]::ParameterName, 'dry-run')
            [CompletionResult]::new('--no-open', '--no-open', [CompletionResultType]::ParameterName, 'no-open')
            [CompletionResult]::new('--no-install', '--no-install', [CompletionResultType]::ParameterName, 'no-install')
            [CompletionResult]::new('-v', '-v', [CompletionResultType]::ParameterName, 'v')
            [CompletionResult]::new('--verbose', '--verbose', [CompletionResultType]::ParameterName, 'verbose')
            [CompletionResult]::new('--quiet', '--quiet', [CompletionResultType]::ParameterName, 'quiet')
            [CompletionResult]::new('--no-color', '--no-color', [CompletionResultType]::ParameterName, 'no-color')
            [CompletionResult]::new('--all', '--all', [CompletionResultType]::ParameterName, 'Alias for the default set')
            [CompletionResult]::new('-h', '-h', [CompletionResultType]::ParameterName, 'Print help')
            [CompletionResult]::new('--help', '--help', [CompletionResultType]::ParameterName, 'Print help')
            [CompletionResult]::new('-V', '-V ', [CompletionResultType]::ParameterName, 'Print version')
            [CompletionResult]::new('--version', '--version', [CompletionResultType]::ParameterName, 'Print version')
            break
        }
    })

    $completions.Where{ $_.CompletionText -like "$wordToComplete*" } |
        Sort-Object -Property ListItemText
}
