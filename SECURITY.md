# Security

dino runs coding agents, holds their API keys and sees their traffic, and its account server holds
accounts and synced settings, so we take reports seriously.

## Reporting a vulnerability

Please don't open a public issue. Report it privately:

- through GitHub: the **Security** tab of this repository → **Report a vulnerability**, or
- by email to security@meetdino.com (TODO: set up this address before going public).

Include what you found, how to reproduce it, and what an attacker could do with it. We'll confirm
we got it within a few days, keep you posted while we fix it, and credit you when it's released if
you'd like.

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
