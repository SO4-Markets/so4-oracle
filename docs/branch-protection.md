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
