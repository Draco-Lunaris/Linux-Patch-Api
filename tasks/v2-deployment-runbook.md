# v2 Deployment Runbook — SUPERSEDED

**Status:** SUPERSEDED — 2026-06-29
**Replaced by:** Manager Pull model per [AGENTS.md](../AGENTS.md) Rules 1-2 and INTERFACE_CONTRACT.md in the Linux-Patch-Manager repo.

This document described the rejected v2 CI-push deployment model (publish-to-manager-repo CI job, GPG key stored in Vaultwarden/CI secrets, https:// repo URLs). That model was logistically impossible and has been replaced.

**Do NOT follow any instructions in this document.** It is retained as a historical artifact only.

For current deployment procedures, see:
- Agent: [DEPLOYMENT_GUIDE.md](../DEPLOYMENT_GUIDE.md)
- Manager-Agent contract: [INTERFACE_CONTRACT.md](../../linux-patch-manager-fresh/INTERFACE_CONTRACT.md) in the Linux-Patch-Manager repo
- GPG key management: `docs/gpg-key-rotation.md` in the Linux-Patch-Manager repo
