// Port of apps/web/src/inbox/types.ts.

export type MessageKind = 'direct' | 'signal';

export type EntityRef = {
  entity_type: string;
  entity_id: string;
  /// SPA-resolvable path. New producers populate it directly so
  /// the inbox renders the link without a type→route dispatcher.
  /// Optional for backward compatibility with messages emitted
  /// before the field landed.
  entity_path?: string | null;
};

export type Message = {
  id: string;
  sender_id: string;
  recipient_id: string;
  subject: string;
  body: string;
  entity_ref: EntityRef | null;
  kind: MessageKind;
  sent_at: string;
  read_at: string | null;
};
