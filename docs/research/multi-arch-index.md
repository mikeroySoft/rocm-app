# Research: multi-arch index enumeration and coverage

Ticket: [#17 Research multi-arch index enumeration and coverage](https://github.com/mikeroySoft/rocm-app/issues/17)
Map: [#15 Wayfinder map: ROCm version control in the UI](https://github.com/mikeroySoft/rocm-app/issues/15)
Date of observation: 2026-07-28. All index contents below were fetched live on that date; index contents drift as new nightlies/releases publish.

## TL;DR

The multi-arch indexes are plain PEP 503 HTML (no JSON API); version enumeration is scraping `rocm-<version>.tar.gz` anchors from the `rocm` project page. **The release multi-arch index carries only 7.13.0 and 7.14.0** — the locked N-2 stable window (7.12.0, 7.11.0) exists only on the legacy per-family indexes. Windows (win_amd64) coverage on multi-arch equals or exceeds legacy for Radeon targets and is the *only* place with 7.14.0 Windows wheels; Instinct targets are Linux-only. "Newest release" is authoritatively GitHub's `releases/latest` on ROCm/TheRock (`therock-7.14` today); no upstream N-2 support-window statement exists — the window is app policy. Pinned installs (`rocm==X` + `torch==Y+rocmX`) are version-consistent because both the `rocm` sdist and the torch wheels carry exact `==` pins on the ROCm side; torch↔torchvision/torchaudio pairing is **not** pin-enforced upstream and still needs client-side matrix matching like rocm-cli does today.

---

## Q1 — Enumerating `rocm` versions from the multi-arch indexes

**Status: ANSWERED**

Both indexes are [PEP 503](https://peps.python.org/pep-0503/) "simple repository" HTML:

- Root page lists one anchor per project: [repo.amd.com/rocm/whl-multi-arch/](https://repo.amd.com/rocm/whl-multi-arch/), [rocm.nightlies.amd.com/whl-multi-arch/](https://rocm.nightlies.amd.com/whl-multi-arch/).
- Each project page lists one anchor per file: [whl-multi-arch/rocm/](https://repo.amd.com/rocm/whl-multi-arch/rocm/).

Mechanics for the app-facing tier list:

1. `GET <index>/rocm/` and parse anchor filenames. The `rocm` project is **sdist-only** (`rocm-<version>.tar.gz`, no wheels — it is the meta package built on the target machine, per [RELEASES.md](https://github.com/ROCm/TheRock/blob/main/RELEASES.md#installing-multi-arch-rocm-python-packages)), so versions come from stripping the `rocm-` prefix and `.tar.gz` suffix. This is exactly what rocm-cli's `load_simple_index_versions` does for the per-family indexes today (`rocm-cli/apps/rocm/src/therock.rs`).
2. **No JSON API**: requesting either index with `Accept: application/vnd.pypi.simple.v1+json` ([PEP 691](https://peps.python.org/pep-0691/)) returns `200` with `Content-Type: text/html` and the same HTML body (verified 2026-07-28 against both `repo.amd.com` and `rocm.nightlies.amd.com`). HTML scraping is the only option.

Observed contents on 2026-07-28:

| Index | `rocm` versions |
| --- | --- |
| [repo.amd.com/rocm/whl-multi-arch/rocm/](https://repo.amd.com/rocm/whl-multi-arch/rocm/) | `7.13.0`, `7.14.0` — nothing else |
| [rocm.nightlies.amd.com/whl-multi-arch/rocm/](https://rocm.nightlies.amd.com/whl-multi-arch/rocm/) | `7.13.0a20260425` … `7.13.0a20260515`, `7.14.0a20260518` … `7.14.0a20260624`, `7.15.0a20260626` … `7.15.0a20260728` (daily alphas, occasional gaps) |

Nightly versions use PEP 440 alpha format `X.Y.ZaYYYYMMDD`, so "latest alpha" = max by PEP 440 ordering (also lexicographic within a stream). Note pip needs `--pre` to select them.

## Q2 — Which versions carry which `rocm-sdk-device-gfx*` packages

**Status: ANSWERED — coverage is NOT uniform, and 7.11.0/7.12.0 do not exist on multi-arch at all**

The dominant fact: the release multi-arch index simply **does not have 7.11.0 or 7.12.0** (see Q1 table). Older releases live only on the legacy per-family indexes — e.g. [whl/gfx120X-all/rocm/](https://repo.amd.com/rocm/whl/gfx120X-all/rocm/) lists `7.10.0`, `7.11.0`, `7.12.0`, `7.13.0` (and, conversely, **not** `7.14.0` — the newest release is multi-arch-only). So the locked tier policy in [#15](https://github.com/mikeroySoft/rocm-app/issues/15) (stable = 7.13.0 / 7.12.0 / 7.11.0 via multi-arch) cannot be satisfied as stated: 7.12.0 and 7.11.0 installs must either stay on legacy per-family indexes or the stable window must be restricted to what multi-arch carries.

Device-project inventory (root page anchors, 2026-07-28):

- Release index: 25 `rocm-sdk-device-gfx*` projects (gfx1010–gfx1036, gfx1100–gfx1103, gfx1150–gfx1153, gfx1200, gfx1201, gfx1250, gfx908, gfx90a, gfx942, gfx950) — [root](https://repo.amd.com/rocm/whl-multi-arch/).
- Nightly index: the same **plus** `gfx900`, `gfx906`, `gfx90c` (28 total) — [root](https://rocm.nightlies.amd.com/whl-multi-arch/).

Per-version coverage spot-checks on the release index (each cell = wheel exists):

| Project | 7.13.0 linux | 7.13.0 win | 7.14.0 linux | 7.14.0 win |
| --- | --- | --- | --- | --- |
| [rocm-sdk-device-gfx1100](https://repo.amd.com/rocm/whl-multi-arch/rocm-sdk-device-gfx1100/) | ✅ | ✅ | ✅ | ✅ |
| [rocm-sdk-device-gfx1151](https://repo.amd.com/rocm/whl-multi-arch/rocm-sdk-device-gfx1151/) | ✅ | ✅ | ✅ | ✅ |
| [rocm-sdk-device-gfx1201](https://repo.amd.com/rocm/whl-multi-arch/rocm-sdk-device-gfx1201/) | ✅ | ✅ | ✅ | ✅ |
| [rocm-sdk-device-gfx950](https://repo.amd.com/rocm/whl-multi-arch/rocm-sdk-device-gfx950/) | ✅ | — | ✅ | — |
| [rocm-sdk-device-gfx1250](https://repo.amd.com/rocm/whl-multi-arch/rocm-sdk-device-gfx1250/) | — | — | ✅ (only) | — |

So within the versions multi-arch does carry, RDNA3/RDNA3.5/RDNA4 consumer targets (everything rocm-cli's families cover today, incl. gfx1201, gfx1100, gfx1151, gfx1200) are uniform across 7.13.0/7.14.0 on both OSes; Instinct targets are Linux-only; brand-new targets appear only from the release where they were added (gfx1250 → 7.14.0+). Nightlies carry device wheels per nightly version including win_amd64 (checked [rocm-sdk-device-gfx1201](https://rocm.nightlies.amd.com/whl-multi-arch/rocm-sdk-device-gfx1201/), through `7.15.0a20260728`).

Caveat from upstream: a device wheel existing ≠ the target working — see the warning and per-target status in [SUPPORTED_GPUS.md](https://github.com/ROCm/TheRock/blob/main/SUPPORTED_GPUS.md) ("Build Passing" vs "Sanity Tested" vs "Release Ready").

## Q3 — Windows coverage: multi-arch vs legacy per-family

**Status: ANSWERED**

Multi-arch (release, [torch project](https://repo.amd.com/rocm/whl-multi-arch/torch/)):

- `win_amd64` torch wheels for `2.9.1+rocm7.13.0`, `2.10.0+rocm{7.13,7.14}.0`, `2.11.0+rocm{7.13,7.14}.0`, `2.12.0+rocm7.14.0`, cp310–cp314. One asymmetry: `2.8.0+rocm7.13.0` is Linux-only and `2.9.1+rocm7.13.0` is Windows-only.
- Device wheels: `win_amd64` for all Radeon/APU targets checked (Q2 table); none for Instinct targets (gfx950/gfx942/gfx90a/gfx908) — consistent with [SUPPORTED_GPUS.md](https://github.com/ROCm/TheRock/blob/main/SUPPORTED_GPUS.md), whose Windows tables are Radeon-only.
- Nightlies: Windows is fully in the stream — the nightly [torch project](https://rocm.nightlies.amd.com/whl-multi-arch/torch/) listed 2328 `win_amd64` vs 2430 `linux_x86_64` files, and nightly device wheels ship `win_amd64` (2026-07-28 counts).
- Upstream status table says ROCm + PyTorch Python packages are "Available" for both Linux and Windows on multi-arch ([RELEASES.md package availability](https://github.com/ROCm/TheRock/blob/main/RELEASES.md#multi-arch-release-status)).

Legacy per-family ([whl/gfx120X-all](https://repo.amd.com/rocm/whl/gfx120X-all/rocm/) as reference):

- [rocm-sdk-core](https://repo.amd.com/rocm/whl/gfx120X-all/rocm-sdk-core/): `win_amd64` wheels for every version 7.10.0–7.13.0.
- [torch](https://repo.amd.com/rocm/whl/gfx120X-all/torch/): `win_amd64` from `2.9.1+rocm7.10.0` onward; the older 2.7.1/2.8.0 streams are Linux-only.

Conclusion: for the Radeon families the app targets, multi-arch Windows coverage matches legacy at 7.13.0 and is the **only** source for 7.14.0 Windows wheels; legacy remains the only source of Windows wheels for 7.10.0–7.12.0. Nothing is lost in the cutover for 7.13.0+.

## Q4 — Authoritative source for the release set / "newest release"

**Status: ANSWERED (support-window sub-question: no upstream statement exists)**

- GitHub releases on [ROCm/TheRock](https://github.com/ROCm/TheRock/releases): tags `therock-7.10` (2025-12-11), `therock-7.11` (2026-02-11), `therock-7.12` (2026-03-26), `therock-7.13` (2026-05-15), `therock-7.14` (2026-07-15); also an older `therock-7.9.0`. None are marked prerelease, and the GitHub `releases/latest` API resolves to `therock-7.14` (queried via `gh api repos/ROCm/TheRock/releases/latest`, 2026-07-28).
- Index contents diverge from the tag set in both directions: multi-arch release index carries only the newest two (7.13.0, 7.14.0); legacy per-family carries 7.10.0–7.13.0 but not 7.14.0. Neither index reproduces the full tag set.
- **No N-2 / support-window statement found upstream**: [RELEASES.md](https://github.com/ROCm/TheRock/blob/main/RELEASES.md) and [SUPPORTED_GPUS.md](https://github.com/ROCm/TheRock/blob/main/SUPPORTED_GPUS.md) say nothing about how many releases stay installable, and a GitHub code search for "support window" in ROCm/TheRock returns nothing. The N-2 window in #15 is app policy, not an upstream contract.

Recommendation for the app: take release **identity and ordering** ("newest release", the tag set) from GitHub releases (`releases/latest` + `therock-*` tags), and take **installability** from the index a given install would actually use — a tier entry is only offerable if its version is present on the chosen index (per Q2, multi-arch alone cannot serve 7.11.0/7.12.0). Observed multi-arch retention (exactly the two newest stable releases today) is UNRESOLVED as a guarantee: nothing upstream documents whether 7.13.0 will remain once 7.15 releases.

## Q5 — Pinned-version install semantics on multi-arch

**Status: ANSWERED (with one caveat vs rocm-cli's current behavior)**

The ROCm side of the stack is exact-pinned by construction:

- The `rocm` sdist computes every dependency as `<dist-package>==<its own version>`: [`templates/rocm/setup.py`](https://github.com/ROCm/TheRock/blob/main/build_tools/packaging/python/templates/rocm/setup.py) builds `install_requires`/`extras_require` from `pkg.get_dist_package_require(...)`, and [`_dist_info.py`](https://github.com/ROCm/TheRock/blob/main/build_tools/packaging/python/templates/rocm/src/rocm_sdk/_dist_info.py) defines that as `get_dist_package_name(...) + f"=={__version__}"`. So `pip install "rocm[libraries,device-gfx1201]==7.14.0"` yields `rocm-sdk-core==7.14.0`, `rocm-sdk-libraries==7.14.0`, `rocm-sdk-device-gfx1201==7.14.0` exactly. Per-target device extras (incl. OS markers so `device-all` only pulls wheels published for your OS) are generated in the same file.
- torch wheels are built with `PYTORCH_EXTRA_INSTALL_REQUIREMENTS = rocm[libraries]==<exact installed rocm-sdk version>` ([`external-builds/pytorch/build_prod_wheels.py`](https://github.com/ROCm/TheRock/blob/main/external-builds/pytorch/build_prod_wheels.py), `install_requirements = [f"rocm[libraries]=={get_rocm_sdk_version()}"]`), and their local version tag encodes the same version (`torch-2.11.0+rocm7.14.0-…`). [RELEASES.md](https://github.com/ROCm/TheRock/blob/main/RELEASES.md#installing-multi-arch-pytorch-python-packages) confirms: torch depends on `rocm[libraries]` and pip will even *downgrade* an installed ROCm to match the torch wheel.
- `torch[device-gfxNNNN]` pulls `amd-torch-device-gfxNNNN` (torch-version-matched kernel pack) which in turn depends on the matching `rocm-sdk-device-gfxNNNN` ([RELEASES.md tip](https://github.com/ROCm/TheRock/blob/main/RELEASES.md#installing-multi-arch-pytorch-python-packages)).

So yes: `pip install --index-url …/whl-multi-arch/ "rocm[libraries,device-gfx1201]==7.13.0" "torch[device-gfx1201]==2.11.0+rocm7.13.0"` resolves a version-consistent stack, equivalent to what rocm-cli's `resolve_pip_runtime_from_index` (`rocm-cli/apps/rocm/src/therock.rs`) assembles per-family today. Pinning only `rocm==X` and leaving torch floating also works — pip backtracks to the newest torch whose `rocm[libraries]==X` pin matches.

**Caveat — torchvision/torchaudio pairing is not pin-enforced.** `build_prod_wheels.py` injects the exact ROCm pin only into the **torch** build; the torchaudio/torchvision build paths add no equivalent exact torch pin. Upstream instead publishes a manual compatibility matrix and tells users to pin all packages explicitly ([RELEASES.md note](https://github.com/ROCm/TheRock/blob/main/RELEASES.md#installing-multi-arch-pytorch-python-packages): "torch==2.11 torchaudio==2.11 torchvision==0.26 apex==1.11.0"). rocm-cli's `select_matching_pip_package_versions` does this pairing client-side (matching `+rocmX.Y.Z` local suffixes plus torch↔vision/audio base-version compatibility) — that logic remains necessary on multi-arch; pip alone won't guarantee a coherent vision/audio pick.

Operational notes: nightlies are PEP 440 alphas, so pinned nightly installs need `--pre` (or an exact `==7.15.0a20260728` pin, which pip accepts without `--pre`).

---

## Implications for the map (#15)

1. The multi-arch cutover as locked ("per-family indexes are legacy") conflicts with the locked stable window: 7.12.0/7.11.0 are only installable from legacy per-family indexes. Either keep legacy resolution for the N-1/N-2 stable entries, or shrink the stable window to multi-arch contents (today: 7.13.0 only, beta floor already 7.13).
2. Tier enumeration = two HTML scrapes (`<index>/rocm/`), plus GitHub `releases/latest` for naming the beta; no JSON API exists.
3. Multi-arch retention is undocumented — the tier builder must treat index contents as ground truth per refresh, not cache the release set.
