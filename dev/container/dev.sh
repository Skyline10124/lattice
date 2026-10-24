#!/usr/bin/env bash
set -euo pipefail

container_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
workspace=$(cd -- "$container_dir/../.." && pwd -P)
container=lattice-dev
image=localhost/lattice-dev:rust-1.96.0
label=io.lattice.workspace

check_owner() {
    local actual
    actual=$(podman inspect --format '{{index .Config.Labels "io.lattice.workspace"}}' "$container")
    if [[ "$actual" != "$workspace" ]]; then
        printf '容器 %s 属于其他工作区：%s\n' "$container" "$actual" >&2
        exit 1
    fi
}

start() {
    if podman container exists "$container"; then
        check_owner
        local expected actual
        expected=$(podman image inspect --format '{{.Id}}' "$image")
        actual=$(podman inspect --format '{{.Image}}' "$container")
        if [[ "$expected" != "$actual" ]]; then
            echo '镜像已更新；请先停止并删除旧 lattice-dev 容器，再重新 start。持久卷可保留。' >&2
            exit 1
        fi
    else
        podman create --name "$container" --hostname lattice-dev \
            --label "$label=$workspace" \
            --init --userns=keep-id:uid=1000,gid=1000 --user 1000:1000 \
            --cap-drop=all --security-opt=no-new-privileges \
            --cpus=2 --memory=5g --pids-limit=4096 \
            --volume "$workspace:/workspace:Z" \
            --volume lattice-dev-cargo:/home/developer/.cargo \
            --volume lattice-dev-target:/workspace/target \
            "$image" > /dev/null
    fi
    if [[ "$(podman inspect --format '{{.State.Running}}' "$container")" != true ]]; then
        podman start "$container" > /dev/null
    fi
}

action=${1:-shell}
if [[ $# -gt 0 ]]; then shift; fi
case "$action" in
    build)
        podman build --platform linux/amd64 --tag "$image" --file "$container_dir/Containerfile" "$container_dir"
        ;;
    start)
        start
        podman ps --filter "name=^${container}$" --format '{{.Names}} {{.Status}}'
        ;;
    shell)
        start
        exec podman exec -it "$container" bash
        ;;
    exec)
        [[ $# -gt 0 ]] || { echo '用法：dev.sh exec <命令> [参数...]' >&2; exit 2; }
        start
        exec podman exec "$container" "$@"
        ;;
    verify)
        start
        exec podman exec "$container" bash /workspace/dev/container/verify.sh
        ;;
    stop)
        check_owner
        podman stop "$container"
        ;;
    status)
        check_owner
        podman ps -a --filter "name=^${container}$" --format '{{.Names}} {{.Status}}'
        ;;
    *)
        echo '用法：dev.sh {build|start|shell|exec <命令>|verify|stop|status}' >&2
        exit 2
        ;;
esac
