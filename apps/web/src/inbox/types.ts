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

/// One kind's rows in the whole inbox, all and unread (boss-messages'
/// `KindCount`).
export type KindCount = Readonly<{ kind: string; all: number; unread: number }>;

/// What the inbox read answers (backlog 74da899d): one page of the
/// narrowed inbox, how many rows the narrowing matches, and the whole
/// inbox counted per kind.
export type InboxPage = Readonly<{
  data: ReadonlyArray<Message>;
  total: number;
  limit: number;
  offset: number;
  kinds: ReadonlyArray<KindCount>;
}>;

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
