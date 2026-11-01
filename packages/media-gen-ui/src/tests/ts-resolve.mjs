// Test-only loader. The app bundler resolves relative imports written without an extension
// (`./engine-error`); plain node does not. Importing this module registers a resolve hook that
// retries an extensionless relative specifier with `.ts`. Import it before the modules under test
// (dynamically, so the hook is in place first). Not a test file: the glob is `*.test.mjs`.

import { registerHooks } from 'node:module';

registerHooks({
  resolve(specifier, context, nextResolve) {
    try {
      return nextResolve(specifier, context);
    } catch (error) {
      const relative = specifier.startsWith('./') || specifier.startsWith('../');
      const hasExtension = /\.[cm]?[jt]sx?$|\.json$/.test(specifier);
      if (error && error.code === 'ERR_MODULE_NOT_FOUND' && relative && !hasExtension) {
        return nextResolve(`${specifier}.ts`, context);
      }
      throw error;
    }
  }
});
