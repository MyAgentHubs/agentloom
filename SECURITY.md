# Security Policy

## Reporting a vulnerability

Use GitHub's [Private Vulnerability Reporting](https://github.com/MyAgentHubs/agentloom/security/advisories/new)
(Security → Report a vulnerability). Do not open a public issue, and do not
report vulnerabilities by email or in a pull request.

**A report must include a proof of concept we can reproduce.** Concretely:

- the exact version or commit you tested,
- your OS and how you installed AgentLoom,
- step-by-step instructions, a script, or a recording that reproduces the
  issue on a clean install,
- what an attacker gains, and what access they need to start.

Reports without a reproducible proof of concept are closed without further
analysis. This includes reports produced by scanners or language models that
describe a plausible-sounding issue without demonstrating it. We are a small
team; the cost of investigating a well-written report that turns out to be
imaginary is the reason other projects have stopped accepting reports at all,
and we'd like to keep this channel open.

We do not run a bug bounty and do not pay for reports. We're glad to credit you
in the advisory if you'd like.

## What to expect

- Acknowledgement within 5 working days.
- An initial assessment within 10 working days.
- If confirmed, we'll agree a disclosure timeline with you. 90 days is our
  default.

## Supported versions

The latest release only. We don't backport fixes.

## Scope

In scope:

- Sandbox or workspace-boundary escapes: an agent reading or writing outside
  the project directory it was scoped to.
- Credential exposure: API keys or tokens leaking out of the OS keychain into
  logs, the database, configuration files, crash dumps, or network requests.
- Unexpected outbound network traffic to anything other than a model provider
  the user explicitly configured.
- Remote code execution triggered by untrusted content — for example, a
  malicious repository, file, or model response causing execution the user did
  not authorise.
- Prompt injection that crosses a security boundary: untrusted content causing
  credential exfiltration, or writes outside the authorised workspace.

Out of scope:

- **Agents running commands and editing files.** That is the product. An agent
  executing a shell command or modifying source code in a project the user
  opened is intended behaviour, not a vulnerability.
- Anything requiring the attacker to already have local access to the user's
  machine or their unlocked keychain.
- Vulnerabilities in model providers, or in third-party CLI tools you have
  chosen to configure.
- Missing hardening flags, best-practice deviations, and scanner output with no
  demonstrated impact.
- Denial of service against your own machine.
- Social-engineering scenarios that require the user to deliberately paste a
  malicious instruction and approve the result.

## A note on the threat model

AgentLoom runs AI agents that execute commands and modify files on your
machine, by design. The security boundary we defend is: *the workspace you
authorised*, *the credentials you stored*, and *the network destinations you
chose*. Reports are most useful when they show one of those three being crossed
without the user's involvement.
