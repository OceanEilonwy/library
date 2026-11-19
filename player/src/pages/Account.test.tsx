import { render, screen } from '@solidjs/testing-library';
import userEvent from '@testing-library/user-event';
import { ServicesContext } from '../services';
import { jsonResponse, makeServices, type Responder } from '../test/services';
import { alice } from '../test/fixtures';
import { AccountGate, validateRegistration } from './Account';

function setup(respond: Responder) {
  const services = makeServices(respond);
  render(() => (
    <ServicesContext value={services}>
      <AccountGate />
    </ServicesContext>
  ));
  return services;
}

describe('sign in', () => {
  test('signs in with a username and password', async () => {
    const services = setup((url, init) => {
      if (url === '/api/account/login') {
        expect(JSON.parse(String(init?.body))).toEqual({ login: 'alice', password: 'correct horse' });
        return jsonResponse(200, alice);
      }
      return jsonResponse(404);
    });
    await userEvent.type(screen.getByLabelText('Username or email'), 'alice');
    await userEvent.type(screen.getByLabelText('Password'), 'correct horse');
    await userEvent.click(screen.getByRole('button', { name: 'Sign in' }));
    await vi.waitFor(() => expect(services.session.status()).toBe('signed-in'));
    expect(services.session.account()?.username).toBe('alice');
  });

  test('explains a wrong password', async () => {
    setup(() => jsonResponse(401, { error: 'Wrong username or password.' }));
    await userEvent.type(screen.getByLabelText('Username or email'), 'alice');
    await userEvent.type(screen.getByLabelText('Password'), 'nope');
    await userEvent.click(screen.getByRole('button', { name: 'Sign in' }));
    expect(await screen.findByRole('alert')).toHaveTextContent("That username or password doesn't match.");
  });

  test('explains that an account is waiting for approval', async () => {
    const services = setup(() =>
      jsonResponse(403, { error: 'Your account is waiting for a librarian to approve it.' }),
    );
    await userEvent.type(screen.getByLabelText('Username or email'), 'newbie');
    await userEvent.type(screen.getByLabelText('Password'), 'password123');
    await userEvent.click(screen.getByRole('button', { name: 'Sign in' }));
    const notice = await screen.findByRole('alert');
    expect(notice).toHaveTextContent('Not approved yet');
    expect(notice).toHaveTextContent('Your account is waiting for a librarian to approve it.');
    expect(notice).not.toHaveTextContent("doesn't match");
    expect(services.session.status()).not.toBe('signed-in');
  });

  test('asks for both fields before sending anything', async () => {
    const services = setup(() => jsonResponse(500));
    await userEvent.click(screen.getByRole('button', { name: 'Sign in' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('Enter your username (or email) and password.');
    expect(services.fetch).not.toHaveBeenCalled();
  });

  test('passes on the rate-limit message', async () => {
    setup(() => jsonResponse(429, { error: 'Too many attempts. Try again in a minute.' }));
    await userEvent.type(screen.getByLabelText('Username or email'), 'alice');
    await userEvent.type(screen.getByLabelText('Password'), 'x');
    await userEvent.click(screen.getByRole('button', { name: 'Sign in' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('Too many attempts.');
  });
});

describe('registration', () => {
  test('checks the rules before sending', async () => {
    const services = setup(() => jsonResponse(500));
    await userEvent.click(screen.getByRole('button', { name: 'Make an account' }));
    await userEvent.type(screen.getByLabelText('Username'), 'al');
    await userEvent.type(screen.getByLabelText('Email'), 'not-an-email');
    await userEvent.type(screen.getByLabelText('Password'), 'short');
    await userEvent.click(screen.getByRole('button', { name: 'Make account' }));
    expect(await screen.findByText('Use 3 to 32 characters.')).toBeInTheDocument();
    expect(screen.getByText(/Enter an email address/)).toBeInTheDocument();
    expect(screen.getByText('Use at least 8 characters.')).toBeInTheDocument();
    expect(screen.getByLabelText('Username')).toHaveAttribute('aria-invalid', 'true');
    expect(services.fetch).not.toHaveBeenCalled();
  });

  test("shows the server's field messages", async () => {
    setup(() =>
      jsonResponse(422, { error: 'Check the highlighted fields.', fields: { username: 'That username is taken.' } }),
    );
    await userEvent.click(screen.getByRole('button', { name: 'Make an account' }));
    await userEvent.type(screen.getByLabelText('Username'), 'alice');
    await userEvent.type(screen.getByLabelText('Email'), 'alice@example.org');
    await userEvent.type(screen.getByLabelText('Password'), 'long enough');
    await userEvent.click(screen.getByRole('button', { name: 'Make account' }));
    expect(await screen.findByText('That username is taken.')).toBeInTheDocument();
  });

  test('says plainly when registration is closed', async () => {
    setup(() => jsonResponse(403, { error: 'Registration is closed.' }));
    await userEvent.click(screen.getByRole('button', { name: 'Make an account' }));
    await userEvent.type(screen.getByLabelText('Username'), 'newbie');
    await userEvent.type(screen.getByLabelText('Email'), 'n@example.org');
    await userEvent.type(screen.getByLabelText('Password'), 'long enough');
    await userEvent.click(screen.getByRole('button', { name: 'Make account' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('Ask the librarian');
  });

  test('asks for an account and waits for a librarian, without signing in', async () => {
    const services = setup((url) =>
      url === '/api/account/register'
        ? jsonResponse(202, {
            status: 'pending',
            message: 'Thanks! A librarian needs to approve your account before you can sign in.',
          })
        : jsonResponse(404),
    );
    await services.session.start();
    await userEvent.click(screen.getByRole('button', { name: 'Make an account' }));
    await userEvent.type(screen.getByLabelText('Username'), 'alice');
    await userEvent.type(screen.getByLabelText('Email'), 'alice@example.org');
    await userEvent.type(screen.getByLabelText('Password'), 'long enough');
    await userEvent.click(screen.getByRole('button', { name: 'Make account' }));
    expect(await screen.findByRole('heading', { name: 'Account requested' })).toBeInTheDocument();
    expect(screen.getByText('A librarian needs to approve it before you can sign in.')).toBeInTheDocument();
    expect(screen.getByRole('status')).toHaveTextContent('Thanks!');
    expect(services.session.status()).toBe('signed-out');
    expect(services.session.account()).toBeNull();

    // Back to the sign-in form.
    await userEvent.click(screen.getByRole('button', { name: 'Back to sign in' }));
    expect(screen.getByRole('heading', { name: 'Sign in' })).toBeInTheDocument();
  });
});

test('validation rules match the contract', () => {
  expect(validateRegistration({ username: 'ok_name.1-2', email: 'a@b', password: '12345678' })).toEqual({});
  expect(validateRegistration({ username: 'bad name', email: 'a@b', password: '12345678' }).username).toMatch(
    /letters/,
  );
  expect(validateRegistration({ username: 'x'.repeat(33), email: 'a@b', password: '12345678' }).username).toBeDefined();
});
