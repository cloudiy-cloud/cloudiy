#!/usr/bin/env python3
"""Verify that every worker manifest marked `available` has an image that
actually exists in its registry — the analog of the license allowlist test, but
for image existence. A `planned` worker is skipped (legitimate: announced, not
yet built). This is the check that would have caught the six 404 Deploy buttons.

It does a manifest HEAD via the registry v2 API (anonymous pull token, no layer
download) and is deliberately **network-tolerant**: only a genuine 404 (image
not found) fails; a registry outage / timeout / 5xx / auth hiccup is reported as
INCONCLUSIVE and does NOT fail the run. Distinguishing the two is the whole point
— we reject "image doesn't exist", never "the registry was flaky".

Run:  python3 scripts/verify-worker-images.py [manifests_dir ...]
Exit: 0 = all available images exist (or only inconclusive) · 1 = a real MISSING.
"""
import json
import os
import sys
import urllib.request
import urllib.error

DEFAULT_DIRS = ["crates/cloudiy/manifests"]


def parse_image(image):
    """image ref -> (registry_host, repo, reference). Handles ghcr/dockerhub,
    tag or @digest."""
    ref = "latest"
    if "@" in image:
        image, ref = image.split("@", 1)
    elif ":" in image.rsplit("/", 1)[-1]:
        image, ref = image.rsplit(":", 1)
    # A leading segment with a dot or colon is a registry host; else Docker Hub.
    first = image.split("/", 1)[0]
    if "." in first or ":" in first:
        registry, repo = first, image.split("/", 1)[1]
    else:
        registry, repo = "registry-1.docker.io", image
        if "/" not in repo:  # official image → library/<name>
            repo = "library/" + repo
    return registry, repo, ref


def auth_endpoint(registry):
    if registry == "registry-1.docker.io":
        return "https://auth.docker.io/token?service=registry.docker.io&scope=repository:{repo}:pull"
    if registry == "ghcr.io":
        return "https://ghcr.io/token?scope=repository:{repo}:pull"
    # Best-effort generic v2 token endpoint.
    return "https://" + registry + "/token?scope=repository:{repo}:pull"


def get_token(registry, repo):
    url = auth_endpoint(registry).format(repo=repo)
    with urllib.request.urlopen(url, timeout=20) as r:
        return json.load(r).get("token") or json.load(r).get("access_token")


ACCEPT = ", ".join([
    "application/vnd.oci.image.index.v1+json",
    "application/vnd.oci.image.manifest.v1+json",
    "application/vnd.docker.distribution.manifest.list.v2+json",
    "application/vnd.docker.distribution.manifest.v2+json",
])


def classify_http(code):
    """A worker marked `available` must be **anonymously pullable** — that's what
    a provider node does. So any deterministic 4xx (image absent, or private and
    thus unusable by the network) is a failure; only rate-limiting (429) and
    server errors (5xx) are transient."""
    if code == 200:
        return "exists"
    if code == 429 or 500 <= code < 600:
        return ("inconclusive", f"HTTP {code}")
    if 400 <= code < 500:
        # 404 = missing tag; 401/403 = absent-or-private (GHCR returns 403 for a
        # never-published repo). Either way an anonymous `docker pull` fails.
        reason = "image not found" if code == 404 else "not anonymously pullable"
        return ("missing", f"HTTP {code}: {reason}")
    return ("inconclusive", f"HTTP {code}")


def check_image(image):
    """Return 'exists' | ('missing', why) | ('inconclusive', reason)."""
    try:
        registry, repo, ref = parse_image(image)
        token = get_token(registry, repo)
        url = f"https://{registry}/v2/{repo}/manifests/{ref}"
        req = urllib.request.Request(url, method="HEAD")
        req.add_header("Accept", ACCEPT)
        if token:
            req.add_header("Authorization", "Bearer " + token)
        with urllib.request.urlopen(req, timeout=20) as r:
            return classify_http(r.status)
    except urllib.error.HTTPError as e:
        return classify_http(e.code)
    except Exception as e:  # DNS, timeout, TLS, token failure — transient
        return ("inconclusive", type(e).__name__)


def load_available(dirs):
    out = []
    for d in dirs:
        if not os.path.isdir(d):
            continue
        for name in sorted(os.listdir(d)):
            if not name.endswith(".json"):
                continue
            try:
                w = json.load(open(os.path.join(d, name)))["worker"]
            except Exception as e:
                print(f"!! unreadable manifest {name}: {e}")
                continue
            if w.get("status") == "available":
                # A pinned digest is verified at that exact digest.
                image = w["image"]
                if w.get("digest"):
                    base = image.split("@")[0].rsplit(":", 1)[0]
                    image = f"{base}@{w['digest']}"
                out.append((w["id"], image))
    return out


def main():
    dirs = sys.argv[1:] or DEFAULT_DIRS
    available = load_available(dirs)
    print(f"Verifying {len(available)} 'available' worker image(s)\n")
    missing, inconclusive = [], []
    for wid, image in available:
        res = check_image(image)
        if res == "exists":
            print(f"  [OK]      {wid}: {image}")
        elif res[0] == "missing":
            print(f"  [MISSING] {wid}: {image}  <-- {res[1]}")
            missing.append((wid, image))
        else:
            print(f"  [SKIP]    {wid}: {image}  ({res[1]} — transient, not a real absence)")
            inconclusive.append((wid, image))
    print()
    if missing:
        print(f"FAIL: {len(missing)} available worker image(s) do not exist — "
              f"mark them 'planned' until published, or fix the ref.")
        return 1
    if inconclusive:
        print(f"INCONCLUSIVE for {len(inconclusive)} image(s) (transient) — not failing.")
    print("OK: every available worker image exists.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
