# Security policy

sealwax processes documents its users did not write, and signs documents others rely on. Security
reports are taken seriously.

## Reporting a vulnerability

**Do not open a public issue.** Report privately through GitHub's private vulnerability
reporting: on the repository's **Security** tab, choose **Report a vulnerability**.

Please include:

- what an attacker can achieve, and under which conditions;
- the version or commit, and how to reproduce: ideally a synthetic PDF, XFDF or certificate, never
  a document holding personal or confidential data;
- any idea you have of a fix.

We acknowledge reports within 7 days, keep you informed of progress, and credit you in the advisory
unless you prefer not to be named. Please give us a reasonable time to release a fix before
disclosing the issue.

## Scope

Among what we consider vulnerabilities:

- a crash, a hang, or memory exhaustion caused by a document or an XFDF, despite the `Limits` an
  operation runs with;
- a signature, timestamp or validation data that sealwax accepts or produces while it should not:
  a path to an untrusted anchor, a revoked certificate taken as valid, a timestamp not checked;
- document content leaking into error messages;
- the library reaching the network, or the file system, on its own.

Bounds the caller explicitly lifts (`Limits::unbounded()`) are the caller's choice.

## Supported versions

sealwax is in early development: only the latest release, and the `main` branch, receive security
fixes.
