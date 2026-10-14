// Approved c6f08b60 D1: reserved synthetic inputs, never a deployment
// default. Tests that pin an estate declaration keep reading that file.
const TEST_NET_1 = [192, 0, 2] as const;
const TEST_NET_2 = [198, 51, 100] as const;

function address(network: readonly number[], lastOctet: number): string {
  if (!Number.isInteger(lastOctet) || lastOctet < 0 || lastOctet > 255) {
    throw new RangeError('fixture address needs an integer octet from 0 through 255');
  }
  return [...network, lastOctet].join('.');
}

export const testNet1 = (lastOctet: number): string => address(TEST_NET_1, lastOctet);
export const testNet2 = (lastOctet: number): string => address(TEST_NET_2, lastOctet);
export const EXAMPLE_DOMAIN = 'example.com';
export const EXAMPLE_HOST = 'host.test';
export const EXAMPLE_EMAIL = `person@${EXAMPLE_DOMAIN}`;
