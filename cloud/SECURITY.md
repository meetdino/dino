# Security

dino-cloud holds dino accounts, their sign-ins and their synced settings, so we take reports
seriously.

## Reporting a vulnerability

Please don't open a public issue. Report it privately:

- through GitHub: the **Security** tab of this repository → **Report a vulnerability**, or
- by email to security@meetdino.com (TODO: set up this address before going public).

Include what you found, how to reproduce it, and what an attacker could do with it. We'll confirm
we got it within a few days, keep you posted while we fix it, and credit you when it's released if
you'd like.

## Supported versions

The latest commit on `main`, which is what dino's own instance (cloud.meetdino.com) runs. If you
host your own, keep it up to date.

## What's in scope

Sign-in (OAuth, device flow, email links and codes), tokens and their revocation, account and
device data, settings sync, rate limits, and anything that lets one account reach another's data.
The dino app and `dinod` are in [dino](https://github.com/asdf9384/dino) and follow the same
policy.
