"""Fail Terraform's setup check if an IAM allow policy includes the sandbox."""

import json
import subprocess
import sys


def gcloud(*args):
    result = subprocess.run(
        ["gcloud", *args, "--format=json", "--quiet"],
        check=True, text=True, stdout=subprocess.PIPE,
    )
    return json.loads(result.stdout)


def check_policy(policy, principals):
    # Reject even conditional grants. Conditions may become true after setup.
    for binding in policy.get("bindings", []):
        for member in binding["members"]:
            if member in principals:
                raise ValueError(f'{binding["role"]} grants access to the sandbox via {member}')


def main():
    request = json.load(sys.stdin)
    project = request["project"]
    number = request["project_number"]
    pool = f"iam.googleapis.com/projects/{number}/locations/global/workloadIdentityPools/{project}.svc.id.goog"
    cluster = f"projects/{project}/locations/{request['location']}/clusters/{request['cluster_name']}"
    namespace = request.get("sandbox_namespace", "runbook-sandboxes")
    account = request.get("sandbox_account", "sandbox")
    principals = {
        "allAuthenticatedUsers",
        f"serviceAccount:{project}.svc.id.goog[{namespace}/{account}]",
        f"principal://{pool}/subject/ns/{namespace}/sa/{account}",
        f"principal://{pool}/kubernetes.serviceaccount.uid/{request['sandbox_uid']}",
        f"principalSet://{pool}/*",
        f"principalSet://{pool}/namespace/{namespace}",
        f"principalSet://{pool}/kubernetes.cluster/https://container.googleapis.com/v1/{cluster}",
    }

    # Read ancestor policies directly as well as resource policies from
    # inventory. The direct reads are independent of inventory update lag.
    scope = f"projects/{project}"
    check_policy(gcloud("projects", "get-iam-policy", project), principals)
    for ancestor in gcloud("projects", "get-ancestors", project):
        kind, resource_id = ancestor["type"], ancestor["id"]
        if kind == "folder":
            check_policy(gcloud("resource-manager", "folders", "get-iam-policy", resource_id), principals)
            if not scope.startswith("organizations/"):
                scope = f"folders/{resource_id}"
        elif kind == "organization":
            check_policy(gcloud("organizations", "get-iam-policy", resource_id), principals)
            scope = f"organizations/{resource_id}"
        elif kind != "project":
            raise ValueError(f"unknown project ancestor type: {kind}")
    # All GKE workload principal forms include the pool name. Search for
    # candidate bindings and match the documented principal strings exactly.
    query = f'policy:"{project}.svc.id.goog" OR policy:allAuthenticatedUsers'
    for asset in gcloud("asset", "search-all-iam-policies", f"--scope={scope}",
                        f"--query={query}", f"--billing-project={project}"):
        check_policy(asset["policy"], principals)
    print(json.dumps({"safe": "true"}))


if __name__ == "__main__":
    try:
        main()
    except (ValueError, KeyError, subprocess.CalledProcessError) as error:
        sys.exit(f"sandbox IAM setup check failed: {error}")
