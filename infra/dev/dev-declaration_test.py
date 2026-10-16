import copy
import importlib.util
import json
from pathlib import Path
import unittest

HERE = Path(__file__).parent

def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module

class DeclarationTests(unittest.TestCase):
    def test_native_defaulted_job_matches_owning_factory(self):
        reader = load("declaration", HERE / "dev-declaration.py")
        job = json.loads((HERE / "tests/fixtures/defaulted-dev-job.json").read_text())
        self.assertTrue(reader.recognized(job))
        for change in (
            lambda j: j["metadata"].update(namespace="boss"),
            lambda j: j["metadata"].update(name="dev-build-foreign"),
            lambda j: j["spec"]["template"]["spec"].update(hostPID=True),
            lambda j: j["spec"]["template"]["spec"].update(hostPID=0),
            lambda j: j["spec"]["template"]["spec"].update(automountServiceAccountToken=True),
            lambda j: j["spec"]["template"]["spec"]["containers"][0]["volumeMounts"].append({"name":"forge-read", "mountPath":"/token"}),
            lambda j: j["spec"]["template"]["spec"]["containers"][0].update(envFrom=[{"secretRef":{"name":"forge-read"}}]),
            lambda j: j["spec"]["template"]["spec"]["containers"][0].update(serviceAccountName="dev-session"),
            lambda j: j["spec"]["template"]["metadata"]["labels"].update({"controller-uid":"foreign"}),
            lambda j: j["spec"].update(activeDeadlineSeconds=999999),
        ):
            with self.subTest(change=change):
                forged = copy.deepcopy(job)
                change(forged)
                self.assertFalse(reader.recognized(forged))
        self.assertFalse(reader.recognized({"metadata":{"labels":job["metadata"]["labels"]}}))

if __name__ == "__main__":
    unittest.main()
