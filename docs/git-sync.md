# Keep runbooks in a Git repository

A repository folder can be the source of truth for a project's runbooks. CI publishes the folder whenever the default branch changes, and pull requests get a check that runs without changing anything. It works with any Git host and needs no app installed.

## Layout

One folder per project, named by the project's slug. Each runbook source sits directly inside, and its `metadata.id` names the runbook:

```
runbooks/
  app/
    rotate-api-keys.yaml
    post-deploy.yaml
  data-pipelines/
    backfill-a-workspace.yaml
```

A repository with a single project can use one folder of `*.yaml` files instead.

## Publishing

```sh
runbook sync push runbooks --each-project   # every project folder
runbook sync push runbooks/app --project app  # one folder
```

A push sends the whole folder in one request:

- **All or nothing.** Every file is compiled first. If any file is invalid, nothing is published, and the CLI lists each file's errors with line numbers (exit code 2).
- **Versions.** A changed runbook gets a new version that records the commit and branch. An unchanged one keeps its version (`unchanged`).
- **New runbooks** are created, and runbooks written in the app with the same slug are taken over (`adopt`).
- **Locked in the app.** A runbook published from Git is read-only in the app; the editor says so. Change it with a pull request.
- **Removing a file** does nothing unless you pass `--prune`, which archives the runbook. Its versions and runs are kept, and a later push of the file brings it back.
- `--dry-run` validates and reports what would change, without changing anything. Use it on pull requests.

The commit and branch come from `--commit` and `--branch`. Without them, the CLI reads `GITHUB_SHA` and `GITHUB_HEAD_REF` / `GITHUB_REF_NAME`, then the checked-out repository.

On Postgres (Docker, Kubernetes) the batch commits in one transaction. On Cloudflare it is applied runbook by runbook after the whole batch has compiled; publishing is idempotent, so re-running a push that failed partway finishes it.

## Starting from the app

To move runbooks you wrote in the app into a repository:

```sh
runbook sync pull runbooks --each-project   # every project you can read
runbook sync pull runbooks/app --project app
```

`pull` writes each runbook's latest published version to `<slug>.yaml`. It skips runbooks that were never published and archived ones. Commit the files, then let CI push them; the first push adopts each runbook.

## Tokens

Sync needs `runbooks:publish` in each project. A service token acts for one project, so create one per project with the editor role:

```sh
runbook token create --kind service --role editor --project app \
  --scope runbooks:read --scope runbooks:publish --name "ci sync app"
```

With `--each-project`, the CLI uses `RUNBOOK_TOKEN_<PROJECT>` for each project when it is set. The slug is upper-cased and `-` becomes `_`, so `data-pipelines` uses `RUNBOOK_TOKEN_DATA_PIPELINES`. Otherwise it uses `RUNBOOK_TOKEN`.

## GitHub Actions

```yaml
name: Runbooks
on:
  pull_request:
    paths: ['runbooks/**']
  push:
    branches: [main]
    paths: ['runbooks/**']

jobs:
  sync:
    runs-on: ubuntu-latest
    env:
      RUNBOOK_HOST: ${{ vars.RUNBOOK_HOST }}
      RUNBOOK_TOKEN_APP: ${{ secrets.RUNBOOK_TOKEN_APP }}
      RUNBOOK_TOKEN_DATA_PIPELINES: ${{ secrets.RUNBOOK_TOKEN_DATA_PIPELINES }}
    steps:
      - uses: actions/checkout@v4
      # Install the runbook CLI here (a release binary, or the server image,
      # which includes it).
      - name: Check (pull request)
        if: github.event_name == 'pull_request'
        run: runbook sync push runbooks --each-project --dry-run
      - name: Publish (main)
        if: github.event_name == 'push'
        run: runbook sync push runbooks --each-project
```

The API behind it is `POST /v1/projects/{p}/sync` (see `api/openapi.draft.json`).
