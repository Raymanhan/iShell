/** zmodem.js ships no types; `src/features/shell/zmodem.ts` types what it uses. */
declare module "zmodem.js" {
  const Zmodem: {
    Sentry: new (options: {
      to_terminal: (octets: number[]) => void;
      sender: (octets: number[]) => void;
      on_retract: () => void;
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      on_detect: (detection: any) => void;
    }) => { consume(bytes: ArrayLike<number>): void };
  };
  export default Zmodem;
}
