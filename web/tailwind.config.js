/**
 * Tailwind config for the Cloudiy static site (replaces the dev Play CDN).
 *
 * Rebuild after adding/changing classes in any page:
 *     ./build-css.sh
 * (or run the npx line inside that script). Output: assets/tailwind.css.
 *
 * `content` scans every page in this folder. Tailwind's purge reads the raw
 * text of each file — including the inline <script> blocks — so any class that
 * appears literally (even inside a JS template string) is kept. The `safelist`
 * below only needs classes that are ASSEMBLED at runtime by concatenation and
 * therefore never appear whole in the source (os.html builds DOM in JS).
 *
 * No custom theme is needed: the brand green (#ccff33) and the fonts are used
 * via arbitrary values (text-[#ccff33]) and plain CSS, not Tailwind config, so
 * the stock theme reproduces the CDN output faithfully.
 */
module.exports = {
  content: ['./*.html'],
  safelist: [
    // Grid column counts are the only class family toggled by value in JS.
    'grid-cols-1', 'grid-cols-2', 'grid-cols-3', 'grid-cols-4',
  ],
  theme: { extend: {} },
  plugins: [],
};
