// HR content types — port of apps/web/src/content/types.ts.

export type BulletinPriority = 'normal' | 'pinned' | 'urgent';

export type Audience = {
  all?: boolean;
  departments?: ReadonlyArray<string>;
  roles?: ReadonlyArray<string>;
};

export type Bulletin = {
  id: string;
  title: string;
  body: string;
  actor_id: string;
  posted_on: string;
  expires_on: string | null;
  priority: BulletinPriority;
  audience: Audience;
  created_at: string;
  updated_at: string;
  dismissed_by_viewer: boolean;
};

export type ManualSection = {
  id: string;
  slug: string;
  parent_slug: string | null;
  title: string;
  body: string;
  sort_order: number;
  audience: Audience;
  current_version: number;
  published: boolean;
  created_at: string;
  updated_at: string;
};
