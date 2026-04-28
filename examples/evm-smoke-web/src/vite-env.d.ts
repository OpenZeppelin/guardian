/// <reference types="vite/client" />

interface Window {
  ethereum?: import('@openzeppelin/guardian-evm-client').Eip1193Provider;
}
