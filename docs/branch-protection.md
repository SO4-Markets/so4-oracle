# Branch protection

## Required settings for `main`

The following branch-protection rules must be enabled on
`SO4-Markets/so4-oracle` → Settings → Branches → `main`:

| Setting | Value |
|---|---|
| **Require a pull request before merging** | ✅ |
| Required approvals | 1 |
| Dismiss stale reviews when new commits are pushed | ✅ |
| Require review from Code Owners | ✅ (see `.github/CODEOWNERS`) |
| **Require status checks to pass before merging** | ✅ |
| Required checks | `ci` (the `ci.yml` workflow job) |
| Require branches to be up to date before merging | ✅ |
| **Do not allow bypassing the above settings** | ✅ |
| **Restrict who can push to matching branches** | maintainers team only |

## Why

Without these rules a contributor can push directly to `main`, bypassing
review and CI.  `CODEOWNERS` assigns review responsibility, but it only
takes effect once the "Require review from Code Owners" setting is on.

## Applying with the GitHub CLI

```sh
gh api repos/SO4-Markets/so4-oracle/branches/main/protection \
  --method PUT \
  --field required_status_checks='{"strict":true,"contexts":["ci"]}' \
  --field enforce_admins=true \
  --field required_pull_request_reviews='{"required_approving_review_count":1,"dismiss_stale_reviews":true,"require_code_owner_reviews":true}' \
  --field restrictions=null
```

> **Note:** `restrictions=null` leaves push access open to all collaborators.
> Restrict further by setting `restrictions.teams` or `restrictions.users`.

## Repository settings (manual checklist, #1441)

These used to live in `.github/settings.yml`, which only takes effect if the
Probot **Settings** app is installed on the `SO4-Markets` organization. It is
not, so the file described intent rather than configuring anything, and the
same settings kept regressing. They are applied by hand instead, from
Settings → General / Code security, and checked with the commands below.

| Setting | Value | Why |
|---|---|---|
| Allow merge commits | ❌ | squash-only keeps `git log --first-parent` linear and revertable (#1272) |
| Allow squash merging | ✅, title = PR title, message = PR body | |
| Allow rebase merging | ❌ | |
| Automatically delete head branches | ✅ | merged branches are cleaned up without housekeeping |
| Secret scanning + push protection | ✅ | |
| Private vulnerability reporting | ✅ | |
| Dependabot security updates | ✅ | |

Apply the merge settings:

```sh
gh api repos/SO4-Markets/so4-oracle --method PATCH \
  --field allow_merge_commit=false \
  --field allow_squash_merge=true \
  --field allow_rebase_merge=false \
  --field squash_merge_commit_title=PR_TITLE \
  --field squash_merge_commit_message=PR_BODY \
  --field delete_branch_on_merge=true
```

Verify everything on this page:

```sh
gh api repos/SO4-Markets/so4-oracle \
  --jq '{allow_merge_commit, allow_squash_merge, allow_rebase_merge,
         squash_merge_commit_title, squash_merge_commit_message,
         delete_branch_on_merge, security_and_analysis}'
gh api repos/SO4-Markets/so4-oracle/branches/main/protection
gh api repos/SO4-Markets/so4-oracle/private-vulnerability-reporting
```

If the Settings app is ever installed, restore a `.github/settings.yml`
generated from this table and make this section point at it, so there is one
source of truth rather than two that drift.
