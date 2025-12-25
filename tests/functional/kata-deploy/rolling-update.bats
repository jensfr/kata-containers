#!/usr/bin/env bats
#
# Copyright (c) 2026 Red Hat, Inc.
#
# SPDX-License-Identifier: Apache-2.0
#
# Test that the preStop hook removes the kata-runtime label during
# DaemonSet rolling updates to prevent the race condition where
# kata workloads could be scheduled during artifact updates.

load "${BATS_TEST_DIRNAME}/../../common.bash"
repo_root_dir="${BATS_TEST_DIRNAME}/../../../"
load "${repo_root_dir}/tests/gha-run-k8s-common.sh"

setup() {
	ensure_yq

	pushd "${repo_root_dir}"

	# Set the latest image, the one generated as part of the PR, to be used as part of the tests
	export HELM_IMAGE_REFERENCE="${DOCKER_REGISTRY}/${DOCKER_REPO}"
	export HELM_IMAGE_TAG="${DOCKER_TAG}"

	# Enable debug for Kata Containers
	export HELM_DEBUG="true"

	# Create the runtime class only for the shim that's being tested
	export HELM_SHIMS="${KATA_HYPERVISOR}"

	# Set the tested hypervisor as the default `kata` shim
	export HELM_DEFAULT_SHIM="${KATA_HYPERVISOR}"

	# Let the Helm chart create the default `kata` runtime class
	export HELM_CREATE_DEFAULT_RUNTIME_CLASS="true"

	export HELM_K8S_DISTRIBUTION="${KUBERNETES}"

	helm_helper

	echo "::group::kata-deploy logs"
	kubectl -n kube-system logs --tail=100 -l name=kata-deploy
	echo "::endgroup::"

	popd
}

@test "Test preStop hook removes kata-runtime label during rolling update" {
	# Skip if not using Rust-based kata-deploy (bash script version doesn't have remove-label command)
	local kata_deploy_pod
	kata_deploy_pod=$(kubectl -n kube-system get pods -l name=kata-deploy -o jsonpath='{.items[0].metadata.name}')
	if ! kubectl -n kube-system exec "${kata_deploy_pod}" -- /usr/bin/kata-deploy --help 2>/dev/null | grep -q "remove-label"; then
		skip "This test requires Rust-based kata-deploy with remove-label command"
	fi

	# Get a worker node that has kata-deploy running on it
	local test_node
	test_node=$(kubectl get nodes --selector='!node-role.kubernetes.io/control-plane' -o jsonpath='{.items[0].metadata.name}' 2>/dev/null)
	if [[ -z "${test_node}" ]]; then
		# Fallback to any node with kata-deploy
		test_node=$(kubectl -n kube-system get pods -l name=kata-deploy -o jsonpath='{.items[0].spec.nodeName}')
	fi

	echo "Testing on node: ${test_node}"

	# Verify node has kata-runtime label before update
	local label_before
	label_before=$(kubectl get node "${test_node}" -o jsonpath='{.metadata.labels.katacontainers\.io/kata-runtime}')
	[[ "${label_before}" == "true" ]]
	echo "Label before update: ${label_before}"

	# Trigger a rolling update by adding an annotation
	kubectl -n kube-system patch daemonset kata-deploy -p '{"spec":{"template":{"metadata":{"annotations":{"test-rolling-update":"'"$(date +%s)"'"}}}}}'

	# Monitor the label during rollout - it should be removed when pod terminates
	local label_removed=false
	local max_attempts=60
	local attempt=0

	echo "Monitoring label during rollout..."
	while [[ ${attempt} -lt ${max_attempts} ]]; do
		local current_label
		current_label=$(kubectl get node "${test_node}" -o jsonpath='{.metadata.labels.katacontainers\.io/kata-runtime}' 2>/dev/null || echo "")

		local pod_status
		pod_status=$(kubectl -n kube-system get pods -l name=kata-deploy --field-selector spec.nodeName="${test_node}" -o jsonpath='{.items[*].status.phase}' 2>/dev/null || echo "")

		echo "Attempt ${attempt}: label='${current_label}' pod_status='${pod_status}'"

		# Check if label was removed at any point during the update
		if [[ -z "${current_label}" || "${current_label}" == "null" ]]; then
			echo "Label was removed during rollout - preStop hook is working!"
			label_removed=true
		fi

		# If we see a Running pod and the label is back, we've completed the cycle
		if [[ "${pod_status}" == "Running" && "${current_label}" == "true" && "${label_removed}" == "true" ]]; then
			echo "Rollout complete: label restored after new pod finished installation"
			break
		fi

		sleep 1
		((attempt++))
	done

	# Verify the label was removed at some point during the update
	[[ "${label_removed}" == "true" ]]

	# Verify label is restored after rollout completes
	kubectl -n kube-system rollout status daemonset/kata-deploy --timeout=120s

	local label_after
	label_after=$(kubectl get node "${test_node}" -o jsonpath='{.metadata.labels.katacontainers\.io/kata-runtime}')
	[[ "${label_after}" == "true" ]]
	echo "Label after update: ${label_after}"
}

teardown() {
	pushd "${repo_root_dir}"
	helm uninstall kata-deploy --ignore-not-found --wait --cascade foreground --timeout 10m --namespace kube-system --debug
	popd
}
