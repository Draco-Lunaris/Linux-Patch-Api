# Packaged Service Integration Test

## Purpose

The `scripts/integration-test.sh` script verifies that the linux-patch-api
package, when installed on a real Ubuntu system with systemd, does not
impose an execution environment that is incompatible with package
management operations.

This test was created after `ProtectKernelModules=true` in the systemd
service file caused boot failures on two machines (Ubuntu 24.04 and 26.04).
The directive made `/usr/lib/modules` inaccessible inside the agent's
cgroup, so `update-initramfs` (run as a dpkg trigger during patch-apply)
generated a broken initramfs without kernel modules. The next boot failed.

## What it tests

1. **Build** — builds the `.deb` package from source.
2. **Install** — installs the `.deb` on the runner.
3. **Unit verification** — verifies the installed service unit matches the
   source unit and contains no prohibited systemd directives.
4. **Service start** — starts `linux-patch-api.service` via systemd.
5. **Environment check** — verifies `/lib/modules/$(uname -r)` and
   `modules.dep` are accessible (the exact path that
   `ProtectKernelModules=true` masked).
6. **Kernel/initramfs regression** — reinstalls the kernel package to
   trigger `update-initramfs`, then verifies:
   - `apt-get` exits successfully
   - `dpkg --audit` produces no output
   - `/boot/vmlinuz-<version>` exists and is non-empty
   - `/boot/initrd.img-<version>` exists and is non-empty
   - `lsinitramfs` can parse the initramfs
   - The initramfs contains modules for the installed kernel
7. **Package-script compatibility** — reinstalls `initramfs-tools` and
   `procps` to exercise trigger scripts (update-initramfs, sysctl) and
   verifies no "missing /lib/modules" errors.
8. **Reboot** — skipped on CI runners (requires a disposable VM). The
   initramfs verification above provides the critical coverage.

## CI integration

The test runs on every pull request and tag push, on self-hosted
Ubuntu 24.04 and 26.04 runners. It is NOT gated behind
`if: startsWith(github.ref, 'refs/tags/v')` — it runs on all PRs.

A failure blocks merging (PR) and releasing (tag).

## Artifacts

All artifacts are uploaded to GitHub Actions and retained for 30 days:

- `runner-info.txt` — hostname, OS, systemd version
- `build.log` — package build output
- `install.log` — package installation output
- `effective-unit.txt` — `systemctl cat` output
- `effective-show.txt` — `systemctl show` output
- `service-start.log` — service start output
- `service-status.txt` — service status
- `env-check.log` — environment check output
- `kernel-reinstall.log` — kernel package reinstall output
- `dpkg-audit.txt` — `dpkg --audit` output
- `lsinitramfs.txt` — initramfs contents listing
- `kernel-test-result.txt` — kernel test summary
- `initramfs-tools-reinstall.log` — initramfs-tools trigger test
- `procps-reinstall.log` — procps/sysctl trigger test
- `journal-lpa.txt` — service journal
- `journal-boot.txt` — full boot journal
- `apt-history.txt` — apt history log
- `apt-term.log` — apt term log
- `dpkg.log` — dpkg log
- `summary.txt` — test summary

## Disposable VM reboot test

For full regression coverage including boot verification, run the
script on a disposable VM (not a CI runner):

```bash
# On a disposable Ubuntu VM:
git clone https://github.com/Draco-Lunaris/Linux-Patch-Api
cd Linux-Patch-Api
scripts/integration-test.sh  # without --no-reboot
```

The script will reboot the VM. After reboot, verify:

```bash
uname -r  # matches the target kernel
journalctl -b | grep -i "linux-patch-api"  # service started cleanly
```