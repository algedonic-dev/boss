-- 20260929030551-a-promotion-ticket-is-spent-once.sql — the single-use
-- half of the gateway-signed promotion ticket (design 2cb6256f; backlog
-- 2a228d0c, car 2).
--
-- The gateway signs a promotion ticket only after BOTH assertions of the
-- /me ceremony verify: the key being promoted, and a break-glass hardware
-- key (boss_core::passkey_promotion::PromotionTicket). boss-people's
-- promote door flips nothing without one, and SPENDS the ticket's nonce
-- here in the flip's own transaction — so a copied ticket, replayed
-- inside its two-minute expiry, is refused rather than answered. A replay
-- the design wants harmless (D7: the gateway re-asks after a failed step
-- write) comes with a FRESH ceremony and so a fresh nonce.
--
-- One row per spent ticket. Nothing reads it but the door's INSERT; a
-- row names the packet, the key's owner and credential it spent, so the
-- table reads on its own as the ledger of promotions asked for. It holds
-- no key material.
CREATE TABLE IF NOT EXISTS passkey_promotion_tickets (
    nonce          TEXT PRIMARY KEY,
    packet_id      TEXT NOT NULL,
    employee_id    TEXT NOT NULL,
    credential_id  TEXT NOT NULL,
    spent_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);
