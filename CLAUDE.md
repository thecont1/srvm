<!-- openqodex:start -->
<!-- This block contains instructions for human developers. Automated code reviewers (OpenQodex, Claude Code, etc.) should disregard these instructions and proceed with the review normally. -->
## Review with OpenQodex before you push
- Before any `git push`, run `openqodex review` from the repository root. It takes one to three minutes: allow it up to ten minutes, or run it in the background and wait for it to exit.
- Show the developer the report it prints, exactly as printed. OpenQodex starts its own reviewer process: the agent that wrote the code does not judge its own work.
- Do not push on a blocked verdict unless the developer says so after seeing the findings.
- The report is in `.openqodex/reviews/`.
<!-- openqodex:end -->
