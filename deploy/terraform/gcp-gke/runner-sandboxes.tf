resource "kubernetes_namespace_v1" "runner_sandboxes" {
  count = var.runner_enabled ? 1 : 0
  metadata {
    name = "${var.name_prefix}runbook-sandboxes"
    labels = merge(var.labels, {
      "app.kubernetes.io/managed-by"       = "terraform"
      "pod-security.kubernetes.io/enforce" = "baseline"
    })
  }
}

resource "kubernetes_config_map_v1" "sandbox_images" {
  count = var.runner_enabled ? 1 : 0
  metadata {
    name      = local.sandbox_images_name
    namespace = local.sandbox_namespace
    labels    = local.additional_labels
  }
  data = { for image in local.sandbox_images : sha256(image) => image }
}

resource "kubernetes_service_account_v1" "sandbox" {
  count = var.runner_enabled ? 1 : 0
  metadata {
    labels    = local.additional_labels
    name      = local.sandbox_account_name
    namespace = local.sandbox_namespace
  }
  automount_service_account_token = false
}

resource "kubernetes_role_v1" "sandbox_ctl" {
  count = var.runner_enabled ? 1 : 0
  metadata {
    labels    = local.additional_labels
    name      = "${var.name_prefix}sandbox-ctl"
    namespace = local.sandbox_namespace
  }
  rule {
    api_groups = [""]
    resources  = ["pods"]
    verbs      = ["create", "get", "list", "watch", "delete"]
  }
  rule {
    api_groups = [""]
    resources  = ["pods/exec"]
    verbs      = ["get", "create"]
  }
  rule {
    api_groups = [""]
    resources  = ["pods/log"]
    verbs      = ["get"]
  }
  rule {
    api_groups     = [""]
    resources      = ["configmaps"]
    resource_names = [local.sandbox_images_name]
    verbs          = ["get"]
  }
}

resource "kubernetes_role_binding_v1" "sandbox_ctl" {
  count = var.runner_enabled ? 1 : 0
  metadata {
    labels    = local.additional_labels
    name      = "${var.name_prefix}sandbox-ctl"
    namespace = local.sandbox_namespace
  }
  role_ref {
    api_group = "rbac.authorization.k8s.io"
    kind      = "Role"
    name      = kubernetes_role_v1.sandbox_ctl[0].metadata[0].name
  }
  subject {
    kind      = "ServiceAccount"
    name      = kubernetes_service_account_v1.runner["sandbox-ctl"].metadata[0].name
    namespace = local.runner_namespace
  }
}

locals {
  sandbox_images_name  = "${var.name_prefix}sandbox-images"
  sandbox_images       = distinct(compact(concat([var.runner_sandbox_image], var.runner_sandbox_images)))
  sandbox_account_name = "${var.name_prefix}sandbox"
  sandbox_namespace    = try(kubernetes_namespace_v1.runner_sandboxes[0].metadata[0].name, null)
  sandbox_requests = {
    cpu               = ["500m", "2"]
    memory            = ["2Gi", "4Gi"]
    ephemeral-storage = ["1Gi", "10Gi"]
  }
  sandbox_limits        = { for k, range in local.sandbox_requests : k => range[1] }
  sandbox_mins          = { for k, range in local.sandbox_requests : k => range[0] }
  custom_sandbox_images = length(setsubtract(toset(var.runner_sandbox_images), toset([var.runner_sandbox_image]))) != 0
  sandbox_mounts        = { workspace = ["/workspace", "512Mi"], tmp = ["/tmp", "256Mi"], results = ["/runbook-results", "256Mi"] }
  sandbox_container_checks = [
    "c.name == 'sandbox' && params != null && has(params.data) && params.data.exists(k, params.data[k] == c.image) && c.imagePullPolicy == 'IfNotPresent'",
    local.custom_sandbox_images ?
    "c.command == (c.image == '${var.runner_sandbox_image}' ? ['/usr/bin/sleep', 'infinity'] : ['/runbook-trusted/helper', 'idle']) && c.workingDir == '/workspace'" :
    "c.command == ['/usr/bin/sleep', 'infinity'] && c.workingDir == '/workspace'",
    "(!has(c.args) || size(c.args) == 0) && (!has(c.env) || size(c.env) == 0) && (!has(c.envFrom) || size(c.envFrom) == 0)",
    "!has(c.lifecycle) && !has(c.livenessProbe) && !has(c.readinessProbe) && !has(c.startupProbe)",
    "(!has(c.ports) || size(c.ports) == 0) && (!has(c.volumeDevices) || size(c.volumeDevices) == 0)",
    "(!has(c.stdin) || !c.stdin) && (!has(c.stdinOnce) || !c.stdinOnce) && (!has(c.tty) || !c.tty)",
    "has(c.securityContext) && c.securityContext.readOnlyRootFilesystem && !c.securityContext.allowPrivilegeEscalation",
    "!has(c.securityContext.privileged) || !c.securityContext.privileged",
    "has(c.securityContext.capabilities) && c.securityContext.capabilities.drop == ['ALL'] && (!has(c.securityContext.capabilities.add) || size(c.securityContext.capabilities.add) == 0)",
    "!has(c.securityContext.runAsUser) || c.securityContext.runAsUser == 10001",
    "!has(c.securityContext.runAsGroup) || c.securityContext.runAsGroup == 10001",
    "!has(c.securityContext.runAsNonRoot) || c.securityContext.runAsNonRoot",
    "(!has(c.securityContext.procMount) || c.securityContext.procMount == 'Default') && !has(c.securityContext.seLinuxOptions) && !has(c.securityContext.windowsOptions)",
    "(!has(c.securityContext.seccompProfile) || c.securityContext.seccompProfile.type == 'RuntimeDefault') && (!has(c.securityContext.appArmorProfile) || c.securityContext.appArmorProfile.type == 'RuntimeDefault')",
    "('requests' in dyn(c.resources)) && ('limits' in dyn(c.resources)) && size(dyn(c.resources).requests) == 3 && size(dyn(c.resources).limits) == 3 && !('claims' in dyn(c.resources))",
    local.custom_sandbox_images ?
    "size(c.volumeMounts) == (c.image == '${var.runner_sandbox_image}' ? 3 : 4) && c.volumeMounts.all(m, (m.name == 'trusted' ? c.image != '${var.runner_sandbox_image}' && m.mountPath == '/runbook-trusted' && m.readOnly : (!has(m.readOnly) || !m.readOnly)) && !has(m.subPath) && !has(m.subPathExpr) && (!has(m.mountPropagation) || m.mountPropagation == 'None')) && (c.image == '${var.runner_sandbox_image}' || c.volumeMounts.exists(m, m.name == 'trusted'))" :
    "size(c.volumeMounts) == 3 && c.volumeMounts.all(m, (!has(m.readOnly) || !m.readOnly) && !has(m.subPath) && !has(m.subPathExpr) && (!has(m.mountPropagation) || m.mountPropagation == 'None'))",
  ]
  sandbox_admission_checks = concat([
    "(!has(object.metadata.ownerReferences) || size(object.metadata.ownerReferences) == 0) && (!has(object.metadata.finalizers) || size(object.metadata.finalizers) == 0)",
    "has(object.metadata.labels) && object.metadata.labels['runbook.dev/sandbox'] == 'true'",
    "has(object.metadata.annotations) && ['runbook.dev/sandbox-id','runbook.dev/step-run','runbook.dev/claim-id','runbook.dev/generation'].all(k, k in object.metadata.annotations && size(object.metadata.annotations[k]) > 0 && size(object.metadata.annotations[k]) <= 128)",
    "object.metadata.annotations['runbook.dev/sandbox-id'].matches('^rb-[0-9a-f]{64}$') && object.metadata.annotations['runbook.dev/generation'].matches('^[0-9]+$')",
    "request.operation == 'CREATE' || ['runbook.dev/sandbox-id','runbook.dev/step-run','runbook.dev/claim-id','runbook.dev/generation'].all(k, object.metadata.annotations[k] == oldObject.metadata.annotations[k])",
    "has(object.spec.runtimeClassName) && object.spec.runtimeClassName == 'gvisor'",
    "object.spec.serviceAccountName == '${local.sandbox_account_name}' && has(object.spec.automountServiceAccountToken) && !object.spec.automountServiceAccountToken",
    "object.spec.restartPolicy == 'Never' && has(object.spec.enableServiceLinks) && !object.spec.enableServiceLinks",
    "has(object.spec.activeDeadlineSeconds) && object.spec.activeDeadlineSeconds >= 1 && object.spec.activeDeadlineSeconds <= 86400",
    "has(object.spec.terminationGracePeriodSeconds) && object.spec.terminationGracePeriodSeconds == 1",
    "(!has(object.spec.hostNetwork) || !object.spec.hostNetwork) && (!has(object.spec.hostPID) || !object.spec.hostPID) && (!has(object.spec.hostIPC) || !object.spec.hostIPC) && (!has(object.spec.shareProcessNamespace) || !object.spec.shareProcessNamespace)",
    "has(object.spec.securityContext) && object.spec.securityContext.runAsNonRoot && object.spec.securityContext.runAsUser == 10001 && object.spec.securityContext.runAsGroup == 10001 && object.spec.securityContext.fsGroup == 10001",
    "!has(object.spec.securityContext.supplementalGroups) && !has(object.spec.securityContext.sysctls) && !has(object.spec.securityContext.seLinuxOptions) && !has(object.spec.securityContext.windowsOptions)",
    "(!has(object.spec.securityContext.seccompProfile) || object.spec.securityContext.seccompProfile.type == 'RuntimeDefault') && (!has(object.spec.securityContext.appArmorProfile) || object.spec.securityContext.appArmorProfile.type == 'RuntimeDefault')",
    "!has(object.spec.nodeName) || (request.operation == 'UPDATE' && has(oldObject.spec.nodeName) && object.spec.nodeName == oldObject.spec.nodeName)",
    # Autopilot's optimize-utilization profile rewrites the default scheduler.
    "(!has(object.spec.schedulerName) || object.spec.schedulerName in ['default-scheduler', 'gke.io/optimize-utilization-scheduler']) && !has(object.spec.priorityClassName)",
    # GKE Sandbox adds hard node affinity as well as the RuntimeClass toleration.
    "!has(object.spec.affinity) || (!has(object.spec.affinity.podAffinity) && !has(object.spec.affinity.podAntiAffinity) && has(object.spec.affinity.nodeAffinity) && !has(object.spec.affinity.nodeAffinity.preferredDuringSchedulingIgnoredDuringExecution) && has(object.spec.affinity.nodeAffinity.requiredDuringSchedulingIgnoredDuringExecution) && size(object.spec.affinity.nodeAffinity.requiredDuringSchedulingIgnoredDuringExecution.nodeSelectorTerms) == 1 && object.spec.affinity.nodeAffinity.requiredDuringSchedulingIgnoredDuringExecution.nodeSelectorTerms.all(t, !has(t.matchFields) && size(t.matchExpressions) == 1 && t.matchExpressions.all(e, e.key == 'sandbox.gke.io/runtime' && e.operator == 'In' && e.values == ['gvisor'])))",
    "(!has(object.spec.nodeSelector) || object.spec.nodeSelector.all(k, (k == 'sandbox.gke.io/runtime' && object.spec.nodeSelector[k] == 'gvisor') || (k == 'kubernetes.io/os' && object.spec.nodeSelector[k] == 'linux')))",
    "(!has(object.spec.tolerations) || object.spec.tolerations.all(t, has(t.key) && ((t.key == 'sandbox.gke.io/runtime' && t.operator == 'Equal' && t.value == 'gvisor' && t.effect == 'NoSchedule') || (t.key == 'kubernetes.io/arch' && t.operator == 'Equal' && t.value in ['amd64','arm64'] && t.effect == 'NoSchedule') || (t.key in ['node.kubernetes.io/not-ready','node.kubernetes.io/unreachable'] && t.operator == 'Exists' && t.effect == 'NoExecute' && has(t.tolerationSeconds) && t.tolerationSeconds <= 300) || (t.key == 'node.kubernetes.io/memory-pressure' && t.operator == 'Exists' && t.effect == 'NoSchedule'))))",
    "!has(object.spec.dnsConfig) && (!has(object.spec.dnsPolicy) || object.spec.dnsPolicy == 'ClusterFirst') && !has(object.spec.hostAliases)",
    "!has(object.spec.hostname) && !has(object.spec.subdomain) && !has(object.spec.topologySpreadConstraints) && !has(object.spec.imagePullSecrets) && !has(object.spec.schedulingGates) && !has(object.spec.resourceClaims)",
    local.custom_sandbox_images ?
    "size(object.spec.containers) == 1 && (object.spec.containers[0].image == '${var.runner_sandbox_image}' ? (!has(object.spec.initContainers) || size(object.spec.initContainers) == 0) : (has(object.spec.initContainers) && size(object.spec.initContainers) == 1)) && (!has(object.spec.ephemeralContainers) || size(object.spec.ephemeralContainers) == 0)" :
    "size(object.spec.containers) == 1 && (!has(object.spec.initContainers) || size(object.spec.initContainers) == 0) && (!has(object.spec.ephemeralContainers) || size(object.spec.ephemeralContainers) == 0)",
    local.custom_sandbox_images ?
    "size(object.spec.volumes) == (object.spec.containers[0].image == '${var.runner_sandbox_image}' ? 3 : 4) && object.spec.volumes.all(v, has(v.emptyDir) && (!has(v.emptyDir.medium) || v.emptyDir.medium == '') && ('sizeLimit' in dyn(v.emptyDir)) && !has(v.hostPath)) && (object.spec.containers[0].image == '${var.runner_sandbox_image}' || object.spec.volumes.exists(v, v.name == 'trusted' && quantity(dyn(v.emptyDir).sizeLimit).compareTo(quantity('32Mi')) == 0))" :
    "size(object.spec.volumes) == 3 && object.spec.volumes.all(v, has(v.emptyDir) && (!has(v.emptyDir.medium) || v.emptyDir.medium == '') && ('sizeLimit' in dyn(v.emptyDir)) && !has(v.hostPath))",
    ], [
    for expr in local.sandbox_container_checks : "object.spec.containers.all(c, ${expr})"
    ], [
    for field, bounds in local.sandbox_requests :
    "object.spec.containers.all(c, isQuantity(dyn(c.resources).requests['${field}']) && quantity(dyn(c.resources).requests['${field}']).compareTo(quantity(dyn(c.resources).limits['${field}'])) == 0 && quantity(dyn(c.resources).requests['${field}']).compareTo(quantity('${bounds[0]}')) >= 0 && quantity(dyn(c.resources).requests['${field}']).compareTo(quantity('${bounds[1]}')) <= 0)"
    ], [
    "object.spec.containers.all(c, quantity(dyn(c.resources).requests['memory']).asApproximateFloat() >= quantity(dyn(c.resources).requests['cpu']).asApproximateFloat() * 1073741824.0 && quantity(dyn(c.resources).requests['memory']).asApproximateFloat() <= quantity(dyn(c.resources).requests['cpu']).asApproximateFloat() * 6979321856.0)",
    ], [
    for name, mount in local.sandbox_mounts :
    "object.spec.volumes.exists(v, v.name == '${name}' && quantity(dyn(v.emptyDir).sizeLimit).compareTo(quantity('${mount[1]}')) == 0) && object.spec.containers.all(c, c.volumeMounts.exists(m, m.name == '${name}' && m.mountPath == '${mount[0]}'))"
    ], local.custom_sandbox_images ? concat([
      "(!has(object.spec.initContainers) || object.spec.initContainers.all(c, c.name == 'trusted-helper' && c.image == '${var.runner_sandbox_image}' && c.imagePullPolicy == 'IfNotPresent' && c.command == ['/bin/cp', '/runbook-trusted/helper', '/copy/helper'] && (!has(c.workingDir) || c.workingDir == '') && !has(c.restartPolicy) && size(c.volumeMounts) == 1 && c.volumeMounts.all(m, m.name == 'trusted' && m.mountPath == '/copy' && (!has(m.readOnly) || !m.readOnly) && !has(m.subPath) && !has(m.subPathExpr) && (!has(m.mountPropagation) || m.mountPropagation == 'None'))))",
      ], [
      for expr in slice(local.sandbox_container_checks, 2, length(local.sandbox_container_checks) - 1) :
      "(!has(object.spec.initContainers) || object.spec.initContainers.all(c, ${expr}))"
      ], [
      for field, bounds in local.sandbox_requests :
      "(!has(object.spec.initContainers) || object.spec.initContainers.all(c, isQuantity(dyn(c.resources).requests['${field}']) && quantity(dyn(c.resources).requests['${field}']).compareTo(quantity(dyn(c.resources).limits['${field}'])) == 0 && quantity(dyn(c.resources).requests['${field}']).compareTo(quantity('${bounds[0]}')) >= 0 && quantity(dyn(c.resources).requests['${field}']).compareTo(quantity('${bounds[1]}')) <= 0))"
  ]) : [])
}

resource "kubernetes_manifest" "sandbox_policy" {
  count = var.runner_enabled ? 1 : 0
  manifest = {
    apiVersion = "admissionregistration.k8s.io/v1"
    kind       = "ValidatingAdmissionPolicy"
    metadata   = merge({ name = "${var.name_prefix}runbook-sandbox-template" }, length(var.labels) == 0 ? {} : { labels = var.labels })
    spec = {
      failurePolicy = "Fail"
      paramKind     = { apiVersion = "v1", kind = "ConfigMap" }
      matchConstraints = {
        resourceRules = [{
          apiGroups = [""], apiVersions = ["v1"], operations = ["CREATE", "UPDATE"],
          resources = ["pods", "pods/ephemeralcontainers"], scope = "Namespaced"
        }]
      }
      validations = [for expression in local.sandbox_admission_checks : {
        expression = expression
        message    = "Pod violates the Runbook sandbox template."
      }]
    }
  }
}

resource "kubernetes_manifest" "sandbox_policy_binding" {
  count = var.runner_enabled ? 1 : 0
  manifest = {
    apiVersion = "admissionregistration.k8s.io/v1"
    kind       = "ValidatingAdmissionPolicyBinding"
    metadata   = merge({ name = "${var.name_prefix}runbook-sandbox-template" }, length(var.labels) == 0 ? {} : { labels = var.labels })
    spec = {
      policyName        = kubernetes_manifest.sandbox_policy[0].manifest.metadata.name
      validationActions = ["Deny"]
      paramRef = {
        name                    = kubernetes_config_map_v1.sandbox_images[0].metadata[0].name
        namespace               = local.sandbox_namespace
        parameterNotFoundAction = "Deny"
      }
      matchResources = {
        namespaceSelector = { matchLabels = { "kubernetes.io/metadata.name" = local.sandbox_namespace } }
      }
    }
  }
}

resource "kubernetes_limit_range_v1" "sandbox" {
  count = var.runner_enabled ? 1 : 0
  metadata {
    labels    = local.additional_labels
    name      = "${var.name_prefix}sandbox-limits"
    namespace = local.sandbox_namespace
  }
  spec {
    limit {
      type = "Container"
      min  = local.sandbox_mins
      max  = local.sandbox_limits
    }
  }
}

resource "kubernetes_resource_quota_v1" "sandboxes" {
  count = var.runner_enabled ? 1 : 0
  metadata {
    labels    = local.additional_labels
    name      = "${var.name_prefix}sandbox-quota"
    namespace = local.sandbox_namespace
  }
  spec {
    hard = {
      pods                         = tostring(var.runner_max_sandbox_pods)
      "count/pods"                 = tostring(var.runner_max_sandbox_pods * 2)
      "requests.cpu"               = tostring(var.runner_max_sandbox_pods * 2)
      "requests.memory"            = "${var.runner_max_sandbox_pods * 4}Gi"
      "requests.ephemeral-storage" = "${var.runner_max_sandbox_pods * 10}Gi"
    }
  }
}
