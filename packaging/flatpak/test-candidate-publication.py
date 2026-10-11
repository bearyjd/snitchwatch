#!/usr/bin/env python3
"""Exercise the exact credential-bearing inline scripts against mocked GitHub APIs."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
import yaml

ROOT = Path(__file__).resolve().parents[2]
SHA = 'a' * 40
WORKFLOW = '.github/workflows/candidate-gui.yml'
REF = f'bearyjd/snitchwatch/{WORKFLOW}@refs/heads/r13-candidate'


def encode(value):
    return (json.dumps(value, sort_keys=True, separators=(',', ':')) + '\n').encode()


def digest(data):
    return hashlib.sha256(data).hexdigest()


HARNESS = r'''
const fs = require('fs');
const spec = JSON.parse(fs.readFileSync(process.argv[2]));
const calls = [];
const missing = () => { const error = new Error('missing'); error.status = 404; throw error; };
const repo = {owner: 'bearyjd', repo: 'snitchwatch'};
const context = {repo, eventName: 'push', ref: 'refs/heads/r13-candidate', ...spec.context};
const core = {exportVariable: (key, value) => {process.env[key] = value;}, info: () => {}};
const github = {rest: {
  repos: {
    getContent: async args => {
      if(args.path === 'vendor/opensnitch') return {data: {sha: spec.submodule}};
      const bytes = Buffer.from(spec.source[args.path], 'base64');
      return {data: {type: 'file', encoding: 'base64', size: bytes.length, content: bytes.toString('base64')}};
    },
    getReleaseByTag: async () => spec.releaseExists ? {data: {id: 1}} : missing(),
    createRelease: async args => {calls.push(['create', args]); spec.tag = {type: 'commit', sha: args.target_commitish}; return {data: {id: 7, html_url: 'candidate-url'}};},
    uploadReleaseAsset: async args => {calls.push(['upload', {name: args.name}]); if (spec.moveTagDuringUpload) spec.tag.sha = 'f'.repeat(40); return {data: {name: args.name, size: args.data.length, digest: spec.badUpload ? 'wrong' : 'sha256:' + require('crypto').createHash('sha256').update(args.data).digest('hex')}};},
    updateRelease: async args => {calls.push(['update', args]); return {data: {}};}
  },
  git: {
    getCommit: async () => ({data: {tree: {sha: spec.tree}}}),
    getRef: async args => {
      if (args.ref.startsWith('heads/')) return {data: {object: {sha: spec.branch}}};
      return spec.tag ? {data: {object: spec.tag}} : missing();
    }
  },
  actions: {
    listWorkflowRuns: async () => ({data: {workflow_runs: spec.runs}}),
    listJobsForWorkflowRun: async () => ({data: {jobs: spec.jobs}})
  }
}};
(async () => {
  try {
    await new Function('github','context','core','require',`return (async () => {${spec.validate}})();`)(github, context, core, require);
    if (spec.afterValidation) {
      fs.appendFileSync(process.env.RUNNER_TEMP + '/candidate-data/' + spec.afterValidation, 'tamper');
    }
    await new Function('github','context','core','require',`return (async () => {${spec.publish}})();`)(github, context, core, require);
    console.log(JSON.stringify({ok:true, calls}));
  } catch(error) {
    console.log(JSON.stringify({ok:false, error:error.message, calls}));
  }
})();
'''


class Publication(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.workflow = yaml.safe_load((ROOT / WORKFLOW).read_text())
        scripts = [step['with']['script'] for step in cls.workflow['jobs']['publish']['steps']
                   if 'with' in step and 'script' in step['with']]
        cls.validate, cls.publish = scripts

    def fixture(self):
        manifest = (ROOT / 'packaging/flatpak/org.snitchwatch.Snitchwatch.system.yml').read_bytes()
        lock = (ROOT / 'Cargo.lock').read_bytes()
        pin = json.loads((ROOT / 'packaging/flatpak/cargo-generator-pin.json').read_text())
        assets = {
            'snitchwatch-system-r13-candidate.flatpak': b'bundle bytes never executed',
            'build.log': b'clean build log', 'system-manifest.yml': manifest,
            'metadata.ini': b'[Environment]\nSNITCHWATCH_SYSTEM_BRIDGE=1\n',
            'resolved-manifest.json': b'{}\n', 'generated-cargo-sources.json': b'[]\n',
        }
        evidence = {'schema': 1, 'source_commit': SHA, 'source_tree': 'b' * 40,
                    'submodule_commit': 'c' * 40, 'workflow_sha': SHA, 'workflow_ref': REF,
                    'run_id': '22', 'run_attempt': '1', 'profile': 'system', 'workflow': WORKFLOW,
                    'workflow_sha256': digest((ROOT / WORKFLOW).read_bytes()),
                    'manifest_sha256': digest(manifest), 'cargo_lock_sha256': digest(lock),
                    'permissions_sha256': digest(assets['metadata.ini']), 'generator': pin,
                    'binary_path': '/app/bin/snitchwatch-kirigami', 'binary_sha256': 'd' * 64,
                    'flatpak_commit': 'e' * 64,
                    'flatpak_inputs': {ref: 'f' * 64 for ref in [
                        'runtime/org.kde.Platform/x86_64/6.11', 'runtime/org.kde.Sdk/x86_64/6.11',
                        'runtime/org.freedesktop.Sdk.Extension.rust-stable/x86_64/25.08']}}
        assets['generator-provenance.json'] = encode({'generator': pin,
            'cargo_lock_sha256': digest(lock), 'generated_sources_sha256': digest(assets['generated-cargo-sources.json'])})
        packet = {'schema': 1, 'repository': 'bearyjd/snitchwatch', 'target': 'r13-candidate',
                  'ref': 'refs/heads/r13-candidate', 'source_commit': SHA, 'workflow_sha': SHA,
                  'workflow_ref': REF, 'run_id': '22', 'run_attempt': '1', 'profile': 'system'}
        import base64
        spec = {'validate': self.validate, 'publish': self.publish, 'context': {},
                'tree': 'b' * 40, 'submodule': 'c' * 40, 'branch': SHA,
                'source': {WORKFLOW: base64.b64encode((ROOT / WORKFLOW).read_bytes()).decode(),
                           'Cargo.lock': base64.b64encode(lock).decode(),
                           'packaging/flatpak/org.snitchwatch.Snitchwatch.system.yml': base64.b64encode(manifest).decode(),
                           'packaging/flatpak/cargo-generator-pin.json': base64.b64encode(encode(pin)).decode()},
                'runs': [{'id': 12, 'head_sha': SHA, 'head_branch': 'r13-candidate', 'event': 'push',
                          'status': 'completed', 'conclusion': 'success', 'run_attempt': 1,
                          'repository': {'full_name': 'bearyjd/snitchwatch'}}],
                'jobs': [{'name': name, 'head_sha': SHA, 'run_attempt': 1, 'conclusion': 'success'}
                         for name in ['check', 'test', 'package-check', 'kirigami', 'candidate-contract']]}
        return assets, evidence, packet, spec

    def exercise(self, mutate=None, *, tamper=None, after=None, env_change=None, file_attack=None):
        assets, evidence, packet, spec = self.fixture()
        if mutate:
            mutate(assets, evidence, packet, spec)
        assets['provenance.json'] = encode(evidence)
        packet['assets'] = {name: {'sha256': digest(data), 'size': len(data)} for name, data in assets.items()}
        raw = encode(packet)
        if tamper == 'packet':
            raw += b' '
        assets['candidate-packet.json'] = raw
        if tamper and tamper != 'packet':
            assets[tamper] += b'tamper'
        with tempfile.TemporaryDirectory(prefix='candidate-publisher-test-') as temp:
            temp = Path(temp)
            data = temp / 'candidate-data'
            data.mkdir()
            for name, content in assets.items():
                (data / name).write_bytes(content)
            if file_attack == 'symlink':
                (data / 'build.log').unlink()
                (data / 'build.log').symlink_to(data / 'metadata.ini')
            elif file_attack == 'hardlink':
                (data / 'build.log').unlink()
                os.link(data / 'metadata.ini', data / 'build.log')
            elif file_attack == 'oversized':
                with (data / 'build.log').open('r+b') as file:
                    file.truncate(64 * 1024 * 1024 + 1)
            elif file_attack == 'empty':
                (data / 'build.log').write_bytes(b'')
            if after:
                spec['afterValidation'] = after
            env = os.environ.copy()
            env.update({'GITHUB_SHA': SHA, 'REVIEWED_SHA': SHA, 'GITHUB_WORKFLOW_SHA': SHA,
                        'GITHUB_WORKFLOW_REF': REF, 'GITHUB_RUN_ID': '22', 'GITHUB_RUN_ATTEMPT': '1',
                        'RUNNER_TEMP': str(temp), 'PACKET_SHA256': digest(encode(packet))})
            if env_change:
                env.update(env_change)
            fixture = temp / 'fixture.json'
            fixture.write_text(json.dumps(spec))
            harness = temp / 'harness.js'
            harness.write_text(HARNESS)
            result = subprocess.run(['node', str(harness), str(fixture)], env=env, text=True,
                                    capture_output=True, timeout=20, check=True)
            return json.loads(result.stdout)

    def refused(self, result):
        self.assertFalse(result['ok'], result)
        self.assertEqual(result['calls'], [], 'refusal must precede all release writes')

    def test_fixed_prerelease_success(self):
        result = self.exercise()
        self.assertTrue(result['ok'], result)
        create = result['calls'][0][1]
        self.assertEqual(create['tag_name'], 'r13-candidate')
        self.assertEqual(create['target_commitish'], SHA)
        self.assertTrue(create['draft'])
        self.assertTrue(create['prerelease'])
        self.assertEqual(create['make_latest'], 'false')
        self.assertEqual(result['calls'][-1][1]['draft'], False)
        self.assertEqual(len(result['calls']), 11)

    def test_packet_identity_substitutions(self):
        for key, wrong in {'schema': 2, 'repository': 'attacker/snitchwatch', 'target': 'v13',
                           'ref': 'refs/heads/main', 'source_commit': 'f' * 40, 'workflow_sha': 'f' * 40,
                           'workflow_ref': REF.replace('r13-candidate', 'main'), 'run_id': '23',
                           'run_attempt': '2', 'profile': 'legacy'}.items():
            with self.subTest(key=key):
                self.refused(self.exercise(lambda a, e, p, s: p.update({key: wrong})))

    def test_provenance_substitutions(self):
        for key, wrong in {'source_commit': 'f' * 40, 'source_tree': 'f' * 40,
                           'submodule_commit': 'f' * 40, 'workflow_sha256': 'f' * 64,
                           'cargo_lock_sha256': 'f' * 64, 'manifest_sha256': 'f' * 64,
                           'permissions_sha256': 'f' * 64, 'profile': 'legacy',
                           'binary_path': '/app/bin/untrusted', 'binary_sha256': 'invalid'}.items():
            with self.subTest(key=key):
                self.refused(self.exercise(lambda a, e, p, s: e.update({key: wrong})))

    def test_hash_tampering(self):
        for name in ['packet', 'snitchwatch-system-r13-candidate.flatpak', 'system-manifest.yml',
                     'metadata.ini', 'generated-cargo-sources.json', 'provenance.json']:
            with self.subTest(name=name):
                self.refused(self.exercise(tamper=name))

    def test_after_validation_tampering(self):
        for name in ['candidate-packet.json', 'snitchwatch-system-r13-candidate.flatpak']:
            with self.subTest(name=name):
                self.refused(self.exercise(after=name))

    def test_ref_target_context(self):
        for change in [{'GITHUB_WORKFLOW_SHA': 'f' * 40}, {'REVIEWED_SHA': 'f' * 40},
                       {'GITHUB_WORKFLOW_REF': REF.replace('r13-candidate', 'main')},
                       {'PACKET_SHA256': 'f' * 64}]:
            with self.subTest(change=change):
                self.refused(self.exercise(env_change=change))
        for context in [{'eventName': 'workflow_dispatch'}, {'eventName': 'pull_request'}, {'eventName': 'schedule'}, {'ref': 'refs/heads/main'},
                        {'repo': {'owner': 'attacker', 'repo': 'snitchwatch'}}]:
            self.refused(self.exercise(lambda a, e, p, s: s.update({'context': context})))

    def test_missing_failed_stale_ci(self):
        variants = [lambda s: s.update({'runs': []}), lambda s: s['runs'][0].update({'conclusion': 'failure'}),
                    lambda s: s['runs'][0].update({'status': 'in_progress'}),
                    lambda s: s['runs'][0].update({'head_sha': 'f' * 40}),
                    lambda s: s.update({'jobs': s['jobs'][:-1]}),
                    lambda s: s['jobs'][0].update({'conclusion': 'skipped'}),
                    lambda s: s['jobs'][0].update({'run_attempt': 2})]
        for i, variant in enumerate(variants):
            with self.subTest(i=i):
                self.refused(self.exercise(lambda a, e, p, s: variant(s)))

    def test_branch_tag_release_refusals(self):
        for change in [{'branch': 'f' * 40}, {'tag': {'type': 'commit', 'sha': 'f' * 40}},
                       {'tag': {'type': 'tag', 'sha': SHA}}, {'releaseExists': True}]:
            with self.subTest(change=change):
                self.refused(self.exercise(lambda a, e, p, s: s.update(change)))

    def test_unknown_packet_path_runtime_generator(self):
        variants = [lambda a, e, p, s: p.update({'accepted': True}),
                    lambda a, e, p, s: a.update({'unexpected.sh': b'exit 0'}),
                    lambda a, e, p, s: e['flatpak_inputs'].update({'runtime/org.kde.Sdk/x86_64/6.11': 'bad'}),
                    lambda a, e, p, s: e['generator'].update({'sha256': 'f' * 64})]
        for i, variant in enumerate(variants):
            with self.subTest(i=i):
                self.refused(self.exercise(variant))

    def test_partial_upload_or_moved_tag_stays_draft(self):
        for change in [{'badUpload': True}, {'moveTagDuringUpload': True}]:
            with self.subTest(change=change):
                result = self.exercise(lambda a, e, p, s: s.update(change))
                self.assertFalse(result['ok'], result)
                self.assertEqual(result['calls'][0][0], 'create')
                self.assertTrue(result['calls'][0][1]['draft'])
                self.assertNotIn('update', [call[0] for call in result['calls']])

    def test_bounded_regular_files_only(self):
        for attack in ['symlink', 'hardlink', 'oversized', 'empty']:
            with self.subTest(attack=attack):
                self.refused(self.exercise(file_attack=attack))

    def test_credential_boundary_and_pr_head(self):
        workflow = self.workflow
        self.assertEqual(workflow['permissions'], {'contents': 'read'})
        self.assertEqual(workflow['jobs']['build']['permissions'], {'contents': 'read'})
        publisher = workflow['jobs']['publish']
        for step in publisher['steps']:
            self.assertNotIn('run', step)
            self.assertNotIn('checkout', step['uses'])
            self.assertRegex(step['uses'], r'@[a-f0-9]{40}$')
        self.assertIn("github.event_name == 'push'", publisher['if'])
        self.assertIn('vars.R13_CANDIDATE_REVIEWED_SHA == github.sha', publisher['if'])
        self.assertNotIn('secrets.', json.dumps(workflow['jobs']['build']))
        ci = yaml.safe_load((ROOT / '.github/workflows/ci.yml').read_text())
        self.assertEqual(ci['permissions'], {'contents': 'read'})
        for job in ci['jobs'].values():
            checkout = next(step for step in job['steps'] if step.get('uses', '').startswith('actions/checkout@'))
            self.assertEqual(checkout['with']['ref'], '${{ github.event.pull_request.head.sha || github.sha }}')
            self.assertEqual(checkout['with']['submodules'], True)
            self.assertFalse(checkout['with']['persist-credentials'])
            self.assertRegex(checkout['uses'], r'@[a-f0-9]{40}$')


if __name__ == '__main__':
    unittest.main()
