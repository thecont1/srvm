complete -c srvm -l port -d 'Start port search at N (0 chooses a free port)' -r
complete -c srvm -l select -d 'Run one app: index, qualified id, or a unique name' -r
complete -c srvm -l dry-run
complete -c srvm -l no-open
complete -c srvm -l no-install
complete -c srvm -s v -l verbose
complete -c srvm -l quiet
complete -c srvm -l no-color
complete -c srvm -l all -d 'Alias for the default set'
complete -c srvm -s h -l help -d 'Print help'
complete -c srvm -s V -l version -d 'Print version'
