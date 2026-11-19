// Sign in and registration. Everything else in the Player needs an account.

import { Show, createSignal, useContext } from 'solid-js';
import { errorMessage, isApiError } from '../api';
import { ServicesContext } from '../services';
import { Icon } from '../components/Icon';

export type AccountMode = 'sign-in' | 'register' | 'requested';

/** Said when the server answers 403 to a right password: the account isn't approved yet. */
export const AWAITING_APPROVAL = 'Your account is waiting for a librarian to approve it.';

/** Client-side checks matching the server's rules (docs/player.md). */
export function validateRegistration(input: {
  username: string;
  email: string;
  password: string;
}): Record<string, string> {
  const errors: Record<string, string> = {};
  const username = input.username.trim();
  if (username.length < 3 || username.length > 32) errors.username = 'Use 3 to 32 characters.';
  else if (!/^[A-Za-z0-9_.-]+$/.test(username))
    errors.username = 'Use letters, numbers, dots, dashes and underscores only.';
  if (!input.email.includes('@')) errors.email = 'Enter an email address, like you@example.org.';
  if (input.password.length < 8) errors.password = 'Use at least 8 characters.';
  return errors;
}

function Field(props: {
  id: string;
  label: string;
  type?: string;
  value: string;
  onInput: (value: string) => void;
  error?: string | undefined;
  autocomplete: string;
  hint?: string;
}) {
  const describedBy = () =>
    [props.hint ? `${props.id}-hint` : '', props.error ? `${props.id}-error` : ''].filter(Boolean).join(' ') ||
    undefined;
  return (
    <div class={['field', { invalid: !!props.error }]}>
      <label for={props.id}>{props.label}</label>
      <input
        id={props.id}
        name={props.id}
        type={props.type ?? 'text'}
        value={props.value}
        onInput={(event) => props.onInput(event.currentTarget.value)}
        autocomplete={props.autocomplete}
        aria-invalid={props.error ? 'true' : undefined}
        aria-describedby={describedBy()}
        required
      />
      <Show when={props.hint}>
        <p class="field-hint" id={`${props.id}-hint`}>
          {props.hint}
        </p>
      </Show>
      <Show when={props.error}>
        <p class="field-error" id={`${props.id}-error`}>
          {props.error}
        </p>
      </Show>
    </div>
  );
}

export function SignInForm(props: { onSwitch: () => void }) {
  const { session } = useContext(ServicesContext);
  const [login, setLogin] = createSignal('');
  const [password, setPassword] = createSignal('');
  const [error, setError] = createSignal<string | null>(null);
  const [waiting, setWaiting] = createSignal<string | null>(null);
  const [busy, setBusy] = createSignal(false);

  async function submit(event: SubmitEvent) {
    event.preventDefault();
    setWaiting(null);
    if (!login().trim() || !password()) {
      setError('Enter your username (or email) and password.');
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await session.signIn(login().trim(), password());
    } catch (failure) {
      if (isApiError(failure) && failure.kind === 'forbidden') {
        setWaiting(failure.message || AWAITING_APPROVAL);
        return;
      }
      setError(
        isApiError(failure) && failure.kind === 'unauthorized'
          ? "That username or password doesn't match. Check them and try again."
          : errorMessage(failure),
      );
    } finally {
      setBusy(false);
    }
  }

  return (
    <form class="account-form" onSubmit={submit} novalidate>
      <h1>Sign in</h1>
      <p class="lede">Music, films and documents from the library need an account.</p>
      <Show when={session.expired()}>
        <p class="notice" role="status">
          You've been signed out. Sign in again to carry on.
        </p>
      </Show>
      <Field id="login" label="Username or email" value={login()} onInput={setLogin} autocomplete="username" />
      <Field
        id="password"
        label="Password"
        type="password"
        value={password()}
        onInput={setPassword}
        autocomplete="current-password"
      />
      <Show when={error()}>
        <p class="form-error" role="alert">
          {error()}
        </p>
      </Show>
      <Show when={waiting()}>
        <div class="notice waiting-notice" role="alert">
          <strong>Not approved yet</strong>
          <p>{waiting()}</p>
          <p class="small">Try again once the librarian has let you in.</p>
        </div>
      </Show>
      <button type="submit" class="button primary wide" disabled={busy()}>
        {busy() ? 'Signing in…' : 'Sign in'}
      </button>
      <p class="switch">
        New here?{' '}
        <button type="button" class="text-button" onClick={() => props.onSwitch()}>
          Make an account
        </button>
      </p>
    </form>
  );
}

export function RegisterForm(props: { onSwitch: () => void; onRequested: (message: string) => void }) {
  const { session } = useContext(ServicesContext);
  const [username, setUsername] = createSignal('');
  const [email, setEmail] = createSignal('');
  const [password, setPassword] = createSignal('');
  const [fields, setFields] = createSignal<Record<string, string>>({});
  const [error, setError] = createSignal<string | null>(null);
  const [busy, setBusy] = createSignal(false);

  async function submit(event: SubmitEvent) {
    event.preventDefault();
    const input = { username: username(), email: email().trim(), password: password() };
    const problems = validateRegistration(input);
    setFields(problems);
    setError(null);
    if (Object.keys(problems).length > 0) return;
    setBusy(true);
    try {
      const result = await session.register(input.username.trim(), input.email, input.password);
      props.onRequested(result.message);
    } catch (failure) {
      if (isApiError(failure) && failure.kind === 'validation') setFields(failure.fields);
      setError(
        isApiError(failure) && failure.kind === 'forbidden'
          ? "New accounts can't be made here right now. Ask the librarian to make one for you."
          : errorMessage(failure),
      );
    } finally {
      setBusy(false);
    }
  }

  return (
    <form class="account-form" onSubmit={submit} novalidate>
      <h1>Make an account</h1>
      <p class="lede">
        Choose a username and password. A librarian approves each new account and decides which items it can open.
      </p>
      <Field
        id="username"
        label="Username"
        value={username()}
        onInput={setUsername}
        autocomplete="username"
        error={fields().username}
        hint="3 to 32 letters, numbers, dots, dashes or underscores."
      />
      <Field
        id="email"
        label="Email"
        type="email"
        value={email()}
        onInput={setEmail}
        autocomplete="email"
        error={fields().email}
      />
      <Field
        id="new-password"
        label="Password"
        type="password"
        value={password()}
        onInput={setPassword}
        autocomplete="new-password"
        error={fields().password}
        hint="At least 8 characters."
      />
      <Show when={error()}>
        <p class="form-error" role="alert">
          {error()}
        </p>
      </Show>
      <button type="submit" class="button primary wide" disabled={busy()}>
        {busy() ? 'Making your account…' : 'Make account'}
      </button>
      <p class="switch">
        Already have one?{' '}
        <button type="button" class="text-button" onClick={() => props.onSwitch()}>
          Sign in
        </button>
      </p>
    </form>
  );
}

/** Shown after registering: nothing to do until a librarian approves the account. */
export function AccountRequested(props: { message: string; onSignIn: () => void }) {
  return (
    <section class="account-form account-requested" aria-labelledby="requested-title">
      <h1 id="requested-title" tabindex="-1" ref={(heading) => queueMicrotask(() => heading.focus())}>
        Account requested
      </h1>
      <p class="lede">A librarian needs to approve it before you can sign in.</p>
      <p role="status">{props.message}</p>
      <button type="button" class="button primary wide" onClick={() => props.onSignIn()}>
        Back to sign in
      </button>
    </section>
  );
}

/** The welcome screen shown to anyone not signed in. */
export function AccountGate(props: { initial?: AccountMode }) {
  const [mode, setMode] = createSignal<AccountMode>(() => props.initial ?? 'sign-in');
  const [requested, setRequested] = createSignal('');
  return (
    <main class="account-gate" id="main">
      <div class="account-brand" aria-hidden="true">
        <span class="brand-mark">
          <Icon name="album" size={40} />
        </span>
      </div>
      <p class="brand-name">Library Player</p>
      <Show when={mode() === 'sign-in'}>
        <SignInForm onSwitch={() => setMode('register')} />
      </Show>
      <Show when={mode() === 'register'}>
        <RegisterForm
          onSwitch={() => setMode('sign-in')}
          onRequested={(message) => {
            setRequested(message);
            setMode('requested');
          }}
        />
      </Show>
      <Show when={mode() === 'requested'}>
        <AccountRequested message={requested()} onSignIn={() => setMode('sign-in')} />
      </Show>
      <a class="back-to-site" href="/" target="_self">
        Back to the catalogue
      </a>
    </main>
  );
}
