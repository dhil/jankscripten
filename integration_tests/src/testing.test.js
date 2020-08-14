const fs = require('fs');
const cp = require('child_process');

const testDir = 'test_data';

function makeTest(filename) {
    test(filename, () => {
        // Note that cp.execSync throws on a non-zero exit code.
        let jsPath = `${testDir}/${filename}`;
        let wasmPath = jsPath.replace(/\.js$/, '.wasm');
        let expectedOutputPath = jsPath.replace(/.js$/, '.txt');
    
        // Add -d to pretty print jankyscript output
        cp.execSync(`../bin/jankscripten compile -o ${wasmPath} ${jsPath}`,
            { stdio: 'inherit' });

        let output = String(cp.execSync(`../bin/run-node ${wasmPath}`)).trim();
    
        let expectedOutput = String(fs.readFileSync(expectedOutputPath)).trim();
        expect(output).toBe(expectedOutput);
        fs.unlinkSync(wasmPath);
    });
}

fs.readdirSync(testDir)
    .filter(filename => filename.endsWith('.js'))
    .forEach(filename => makeTest(filename));
