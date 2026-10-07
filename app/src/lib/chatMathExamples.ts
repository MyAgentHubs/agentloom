const kl = String.raw`D_{\mathrm{KL}}(P \parallel Q) \neq D_{\mathrm{KL}}(Q \parallel P)`;

// Product examples shown from the composer, using the same renderer as chat.
export const chatMathExamples = [
  `$${kl}$`,
  `$$${kl}$$`,
  String.raw`\(${kl}\)`,
  String.raw`\[${kl}\]`,
  `$\\text{${"abcdefghij".repeat(40)}}$`,
  String.raw`$\frac{$`,
];
