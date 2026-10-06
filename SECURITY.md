# Security

dino runs coding agents, holds their API keys and sees their traffic, and its account server holds
accounts and synced settings, so we take reports seriously.

## Reporting a vulnerability

Please don't open a public issue, pull request or discussion. Report it privately, through
GitHub: [**Report a vulnerability**](https://github.com/meetdino/dino/security/advisories/new)
(the repository's **Security** tab → **Report a vulnerability**). Only the maintainers see it, and
we work on the fix with you there, in a private fork when it needs one.

Include what you found, how to reproduce it, and what an attacker could do with it. We'll confirm
we got it within a few days, keep you posted while we fix it, and publish an advisory crediting
you when the fix is released, if you'd like.

## Supported versions

Only the latest release gets security fixes. dino updates itself, and `dino --version` says which
you have. For the account server, the latest commit on `main`, which is what dino's own instance
(cloud.meetdino.com) runs; if you host your own, keep it up to date.

## What's in scope

dino, `dinod` and Dino.app from this repository: the local socket and its permissions, the proxy
and the keys it handles, sign-in and settings sync, updates and their signatures, and anything
that lets one local user or a website reach another user's sessions or agents. The account server
in `cloud/`: sign-in (OAuth, device flow, email links and codes), tokens and their revocation,
account and device data, settings sync, rate limits, and anything that lets one account reach
another's data.
