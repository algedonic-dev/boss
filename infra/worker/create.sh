#!/usr/bin/env bash
# --------------------------------------------------------------------------
# Boss — os-worker-1 provisioning (Azure, private-only)
#
# Creates a VNet-internal Azure VM named os-worker-1. No public IP.
# Reachable from os-manager-1 over the private VNet. SSH from outside
# must go through os-manager-1 as a bastion (ssh -J).
#
# Review before running. Creating a VM costs money.
#
# Required environment:
#   BOSS_RESOURCE_GROUP  — Azure resource group (must exist)
#   BOSS_LOCATION        — Azure region (e.g. eastus, westus2)
#   BOSS_SSH_KEY         — path to SSH public key to install on the VM
#   BOSS_VNET            — VNet name (same VNet os-manager-1 is on)
#   BOSS_SUBNET          — subnet name inside that VNet
#
# Optional:
#   BOSS_VM_SIZE      — Azure VM size (default: Standard_B2s)
#   BOSS_IMAGE        — image URN (default: Ubuntu 24.04 LTS)
#   BOSS_ADMIN_USER   — admin username (default: azureuser)
# --------------------------------------------------------------------------
set -euo pipefail

: "${BOSS_RESOURCE_GROUP:?BOSS_RESOURCE_GROUP required}"
: "${BOSS_LOCATION:?BOSS_LOCATION required (e.g. eastus)}"
: "${BOSS_SSH_KEY:?BOSS_SSH_KEY required (path to public key)}"
: "${BOSS_VNET:?BOSS_VNET required (name of VNet that os-manager-1 is on)}"
: "${BOSS_SUBNET:?BOSS_SUBNET required (subnet name inside BOSS_VNET)}"

BOSS_VM_SIZE="${BOSS_VM_SIZE:-Standard_B2s}"
BOSS_IMAGE="${BOSS_IMAGE:-Canonical:ubuntu-24_04-lts:server:latest}"
BOSS_ADMIN_USER="${BOSS_ADMIN_USER:-azureuser}"
VM_NAME="os-worker-1"

if [[ ! -f "${BOSS_SSH_KEY}" ]]; then
    echo "ERROR: SSH key not found at ${BOSS_SSH_KEY}" >&2
    exit 1
fi

echo "==> Creating VM ${VM_NAME} in ${BOSS_RESOURCE_GROUP} (${BOSS_LOCATION})"
echo "    VNet: ${BOSS_VNET}, subnet: ${BOSS_SUBNET}, no public IP"
az vm create \
    --resource-group "${BOSS_RESOURCE_GROUP}" \
    --name "${VM_NAME}" \
    --image "${BOSS_IMAGE}" \
    --size "${BOSS_VM_SIZE}" \
    --admin-username "${BOSS_ADMIN_USER}" \
    --ssh-key-values "${BOSS_SSH_KEY}" \
    --location "${BOSS_LOCATION}" \
    --vnet-name "${BOSS_VNET}" \
    --subnet "${BOSS_SUBNET}" \
    --public-ip-address "" \
    --nic-delete-option Delete \
    --os-disk-delete-option Delete \
    --output table

PRIVATE_IP=$(az vm show \
    --resource-group "${BOSS_RESOURCE_GROUP}" \
    --name "${VM_NAME}" \
    --show-details \
    --query privateIps \
    --output tsv)

echo
echo "=========================================="
echo " os-worker-1 provisioned (private-only)"
echo " Private IP: ${PRIVATE_IP}"
echo "=========================================="
echo
echo "Next steps:"
echo "  1. Update crates/core/boss-core/src/hosts.rs: replace 'TBD' with ${PRIVATE_IP}"
echo "  2. Test SSH from os-manager-1: ssh ${BOSS_ADMIN_USER}@${PRIVATE_IP}"
echo "     (or from workstation via jumphost: ssh -J ${BOSS_ADMIN_USER}@os-manager-1 ${BOSS_ADMIN_USER}@${PRIVATE_IP})"
echo "  3. On worker: clone repo, run infra/cybernetics/setup.sh with BOSS_ROLE=worker"
