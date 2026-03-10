# ADR-0027: OCI Artifact Distribution for .bca Artifacts

**Status:** Accepted
**Date:** 2026-03-09

## Context

The Barbacane data plane requires a compiled `.bca` artifact at startup (`serve --artifact /config/api.bca`). In Kubernetes deployments, this artifact must be provisioned into the pod before the gateway starts.

Several distribution approaches exist:

- **ConfigMap/Secret:** Limited by 1MB etcd object size; not suitable for large artifacts
- **PersistentVolume:** Requires external CI/CD to populate the volume; ties artifact lifecycle to cluster infrastructure
- **Init container (container image):** Works but repurposes container image semantics — mixing application images with data artifacts
- **OCI artifacts (ORAS):** Stores the `.bca` as a typed, versioned artifact in any OCI-compatible registry, cleanly separated from container images

Barbacane already uses OCI registries for container images (ADR-0019). Using the same registry infrastructure for `.bca` artifacts gives users a single place to manage all Barbacane-related distribution assets.

## Decision

`.bca` artifacts will be distributed as OCI artifacts using the [ORAS](https://oras.land) project tooling, with a dedicated media type:

```
application/vnd.barbacane.bca.v1
```

**Pushing an artifact:**
```bash
oras push ghcr.io/my-org/my-api:1.0.0 \
  --artifact-type application/vnd.barbacane.bca.v1 \
  api.bca:application/vnd.barbacane.bca.v1
```

**In Kubernetes (Helm chart):** The data plane Deployment includes an init container running the official `oras` image that pulls the artifact into a shared `emptyDir` volume. The main container mounts the same volume.

> **Note:** ADR-0019 deferred Helm chart creation until deployment patterns stabilized. This ADR describes the init container pattern that informed the Helm chart design; the chart itself now lives in `barbacane-helm-chart/`.

```yaml
initContainers:
  - name: artifact-fetcher
    image: ghcr.io/oras-project/oras:v1.2.0  # minimum version, see Security below
    args:
      - pull
      - $(ARTIFACT_REF)
      - --output
      - /artifact
      - --registry-config
      - /registry-config/.dockerconfigjson
    env:
      - name: ARTIFACT_REF
        value: ghcr.io/my-org/my-api@sha256:abc123...  # set via Helm values
    securityContext:
      runAsNonRoot: true
      runAsUser: 65532
      runAsGroup: 65532
      allowPrivilegeEscalation: false
      readOnlyRootFilesystem: true
      capabilities:
        drop: ["ALL"]
    volumeMounts:
      - name: artifact
        mountPath: /artifact
      - name: registry-config
        mountPath: /registry-config
        readOnly: true
containers:
  - name: barbacane
    args: ["serve", "--artifact", "/artifact/api.bca"]
    securityContext:
      runAsNonRoot: true
      runAsUser: 65532
      runAsGroup: 65532
    volumeMounts:
      - name: artifact
        mountPath: /artifact
        readOnly: true
volumes:
  - name: artifact
    emptyDir:
      sizeLimit: 100Mi  # tune via Helm values for largest expected artifact
  - name: registry-config
    secret:
      secretName: artifact-registry-credentials  # Docker config secret for oras pull
```

**Registry credentials:** `imagePullSecrets` on the Pod spec covers pulling the `oras` init container image itself. For artifact pulls, the `oras` CLI needs credentials injected separately — here via a mounted Docker config secret (`--registry-config`). Both the init container and the main container run as UID 65532 (`nonroot`) per ADR-0019, ensuring the artifact file is readable by both.

### Security

The init container image must be pinned to a **minimum version of `v1.2.0`** in the Helm chart. This version introduced:
- Verified digest-based pulls (preventing tag mutation attacks)
- Improved TLS certificate validation

In production, the Helm chart defaults to a pinned digest (`ghcr.io/oras-project/oras@sha256:...`) rather than a floating tag, with the tag kept as a human-readable comment. The chart maintainers are responsible for updating the pinned digest when upgrading the `oras` image version. Users overriding the image are responsible for maintaining this guarantee in their own values.

Digest-pinned artifact pulls also provide an integrity guarantee that complements ADR-0021's provenance requirements: the pull is content-addressed, ensuring the artifact bytes match what was originally pushed.

### Future: `barbacane compile --push`

As a convenience, a future `barbacane compile --push <registry-ref>` subcommand could combine compilation and artifact publication in a single step, wrapping `oras push` with the correct media type. This would eliminate the need for users to know the media type string and reduce CI/CD boilerplate. This is deferred until the Helm chart deployment pattern is validated in practice.

### What we are NOT doing

- No custom ORAS client — we use the official `oras` CLI image
- No `barbacane` push tooling in this iteration — `oras push` is sufficient

## Consequences

### Easier

- **Versioning:** Artifact versions are explicit OCI tags, not filesystem paths
- **Reuse:** Same registry, same auth, same tooling as container images
- **Auditability:** Registry logs artifact pulls alongside image pulls
- **Provenance:** Digest-pinned pulls are content-addressed, providing integrity guarantees that tie directly into ADR-0021's provenance chain. Artifact hash and metadata can be attached as OCI annotations

### Harder

- **Init container dependency:** Pods cannot start if the registry is unreachable at startup
- **ORAS tooling required:** CI/CD pipelines must have `oras` available to push artifacts
- **Cold start latency:** Large artifacts add pull time to pod startup
- **Image pinning maintenance:** The pinned digest in the Helm chart must be updated when the `oras` image is upgraded

## Related ADRs

- [ADR-0019: Packaging and Release Strategy](0019-packaging-and-release-strategy.md) — OCI registry strategy
- [ADR-0021: Config Provenance](0021-config-provenance.md) — artifact fingerprinting and hash verification

## References

- [ORAS project](https://oras.land)
- [OCI Image Spec — Artifact guidance](https://github.com/opencontainers/image-spec/blob/main/manifest.md)
- [ORAS v1.2.0 release notes](https://github.com/oras-project/oras/releases/tag/v1.2.0)
