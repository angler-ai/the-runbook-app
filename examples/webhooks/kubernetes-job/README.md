# Kubernetes job launcher

[`job.yaml`](job.yaml) runs one agent session for one execution:

- an **init container** sees the project service token (Secret
  `runbook-service-token`) and runs `runbook run token "$EXECUTION_ID"`,
  which stores the execution-scoped access/refresh pair in the CLI's 0600
  credential store on an in-memory volume and prints no secret;
- the **agent container** shares that volume but never sees the service
  token: it can act on this one execution only, reads the resume brief and
  exits when no work is ready. Its tools are the runbook CLI and reads
  inside its working directory (`Read(./**)` in `/work`), so it cannot read
  the stored pair under `/runbook`.

## Launching it

Something has to receive `agent.work_available` and create the Job. Reuse
the [GitHub Actions relay](../github-actions-relay/relay.py): keep its
signature check, stale-timestamp check and dedupe, and replace `dispatch()`
with creating the Job, either through the Kubernetes API from a pod whose
ServiceAccount may only `create` Jobs in this namespace, or with
`kubectl`:

```sh
# Images by digest only, e.g. registry.example/runbook-cli@sha256:<64 hex>.
for image in "$RUNBOOK_CLI_IMAGE" "$AGENT_IMAGE"; do
  [[ "$image" =~ @sha256:[0-9a-f]{64}$ ]] || { echo "pin $image by digest" >&2; exit 1; }
done
slug=$(echo "$EVENT_ID" | tr 'A-Z_' 'a-z-')
sed -e "s/__EXECUTION_ID__/$EXECUTION_ID/g" -e "s/__EVENT_ID__/$EVENT_ID/g" \
    -e "s/__EVENT_SLUG__/$slug/g" \
    -e "s|__RUNBOOK_CLI_IMAGE__|$RUNBOOK_CLI_IMAGE|g" -e "s|__AGENT_IMAGE__|$AGENT_IMAGE|g" \
    job.yaml | kubectl create -f -
```

Both images are pinned by digest (`repository@sha256:...`): the init
container holds the service token, so a mutable tag such as `:latest` would
hand it to whatever the tag points at next. There is no public Runbook
image; build one with the CLI (the repository's `Dockerfile` builds
`runbook-pilot`, which contains it) and push it to your registry.

Naming the Job after the event ID makes a redelivered event a harmless
`AlreadyExists`, but two different events for one run (say, a timer and a
decision) can start two pods for the same execution. That is safe: claims
let only one session work a step and the other finds no ready work and
exits, but it costs a pod. Validate `EXECUTION_ID` (`^exe_[0-7][0-9A-HJKMNP-TV-Z]{25}$`)
and `EVENT_ID` before substituting them. To run at most one session per
execution, have the launcher skip creation while a Job labelled
`therunbook.app/execution=$EXECUTION_ID` is still active.

Setup:

```sh
kubectl create secret generic runbook-service-token --from-literal=token=trb_svc_...
kubectl create secret generic agent-credentials --from-literal=anthropic-api-key=...
kubectl create configmap runbook --from-literal=host=https://runbook.example.com
```
