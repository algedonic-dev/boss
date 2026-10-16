"""Inert private enrollment seam (6f3ef63c).

The gateway must authenticate the human and resolve a presence-signed scope
through its existing session/presence doors before using this seam. Neither
adapter is installed here. Provider confirmation and authentication bytes
remain inside the private broker, never in a Job, event or this public result.
No provider command, HTTP route, credential installation or activation exists
in this module. The uninstalled adapter refuses rather than simulate success.
An unavailable receipt establishes no private operation outcome: only the
private broker may reconcile interrupted work. This seam never retries it.
"""
from asyncio import CancelledError
from dataclasses import dataclass
from typing import Protocol
from uuid import UUID


@dataclass(frozen=True)
class Human:
    employee_id: str


@dataclass(frozen=True)
class ApprovedScope:
    packet_id: UUID
    employee_id: str
    presence_id: UUID
    expected_principal: str


@dataclass(frozen=True)
class OpaqueReceipt:
    receipt_id: UUID


class ScopePort(Protocol):
    async def authorize(self, human: Human, packet: UUID) -> ApprovedScope:
        """Read the original human presence stamp and exact admitted scope."""


class PrivateBrokerPort(Protocol):
    async def begin_private(self, scope: ApprovedScope) -> OpaqueReceipt:
        """Own the authenticated private consent channel; return only its id."""


class UninstalledBroker:
    async def begin_private(self, scope: ApprovedScope) -> OpaqueReceipt:
        raise RuntimeError("private_broker_unavailable")


@dataclass(frozen=True)
class Result:
    receipt: OpaqueReceipt | None = None
    refusal: str | None = None

    def public_record(self) -> dict[str, str]:
        if self.receipt is not None:
            return {"broker_receipt_id": str(self.receipt.receipt_id)}
        return {"refusal": self.refusal or "private_broker_unavailable"}


async def begin(human: Human, packet: UUID, scopes: ScopePort,
                broker: PrivateBrokerPort) -> Result:
    if (type(human) is not Human or type(human.employee_id) is not str
            or not human.employee_id.strip() or type(packet) is not UUID):
        return Result(refusal="human_required")
    try:
        scope = await scopes.authorize(human, packet)
    except (Exception, CancelledError):
        # Never render a private adapter exception or provider response.
        return Result(refusal="scope_unavailable")
    if (type(scope) is not ApprovedScope
            or type(scope.packet_id) is not UUID
            or type(scope.employee_id) is not str
            or type(scope.presence_id) is not UUID
            or scope.presence_id.int == 0
            or type(scope.expected_principal) is not str
            or not scope.expected_principal.strip()
            or scope.packet_id != packet or scope.employee_id != human.employee_id):
        return Result(refusal="scope_mismatch")
    try:
        receipt = await broker.begin_private(scope)
    except (Exception, CancelledError):
        return Result(refusal="private_broker_unavailable")
    if (type(receipt) is not OpaqueReceipt or type(receipt.receipt_id) is not UUID
            or receipt.receipt_id.int == 0):
        return Result(refusal="private_broker_unavailable")
    return Result(receipt=receipt)
