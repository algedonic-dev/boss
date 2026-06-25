// Shipping domain. Port of apps/web/src/shipping/types.ts.

export type ShipmentDirection = 'inbound' | 'outbound';
export type ShipmentStatus =
  | 'label-created'
  | 'picked-up'
  | 'in-transit'
  | 'delivered'
  | 'exception';
export type Carrier = 'fedex' | 'ups' | 'freight' | 'local-pickup';

export type Shipment = {
  id: string;
  direction: ShipmentDirection;
  status: ShipmentStatus;
  /// Absent when no carrier is chosen yet — identity-first shipments
  /// can exist before a label is purchased, and the API omits the field
  /// for them (serde skip_serializing_if). Render with a fallback.
  carrier: Carrier | null;
  tracking_number: string | null;
  origin: string;
  destination: string;
  asset_ids: ReadonlyArray<string>;
  po_id: string | null;
  order_id: string | null;
  account_id: string | null;
  created_on: string;
  shipped_on: string | null;
  estimated_delivery: string | null;
  delivered_on: string | null;
};

export const DIRECTION_LABEL: Record<ShipmentDirection, string> = {
  inbound: 'Inbound',
  outbound: 'Outbound',
};

export const STATUS_LABEL: Record<ShipmentStatus, string> = {
  'label-created': 'Label created',
  'picked-up': 'Picked up',
  'in-transit': 'In transit',
  delivered: 'Delivered',
  exception: 'Exception',
};

export const CARRIER_LABEL: Record<Carrier, string> = {
  fedex: 'FedEx',
  ups: 'UPS',
  freight: 'Freight',
  'local-pickup': 'Local pickup',
};
